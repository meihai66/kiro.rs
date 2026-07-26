//! 自动上号：定时轮询发卡站接口，拉到新的 Kiro API Key 就自动入池
//!
//! 上游接口约定（`config.keyPoll.apiUrl`，默认 `https://key.dnf9999.com/api/my/keys`）：
//!
//! ```text
//! GET {apiUrl}  -H "X-API-Key: usr-xxx"
//! → {"count":5,"active":3,"keys":[{"key":"ksk_...","status":"active","order_id":"..","created_at":".."}]}
//! ```
//!
//! 流程：拉取 → 按 `status` 过滤（可配）→ 与本地凭据比对去重 → 只对新 Key 走
//! import-token-json 管线（逐条验证 + 自动分配/绑定代理）→ **直接启用参与调度**
//! （`force_enable`，忽略「导入默认禁用」；绑定代理失败的凭据仍保持禁用）。
//!
//! 每次轮询落一条 `key_poll_logs`（含原始响应），每个成功上号的 Key 落一条
//! `key_onboard_logs`（凭据 id、脱敏 Key、订单号、所绑代理）。

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::admin::AdminService;
use crate::admin::types::{ImportAction, ImportItems, ImportTokenJsonRequest, TokenJsonItem};
use crate::common::utf8::floor_char_boundary;
use crate::model::config::KEY_POLL_MIN_INTERVAL_SECS;
use crate::storage::{KeyOnboardLogInsert, KeyPollLogInsert};

/// 落库的原始响应上限（超出截断并标注）
pub const KEY_POLL_LOG_MAX_BODY_BYTES: usize = 32 * 1024;

/// 拉取超时（秒）
const FETCH_TIMEOUT_SECS: u64 = 20;

/// 全局轮询互斥：后台定时任务与管理界面「立即上号」可能同时触发，
/// 而「查去重 → 调上游验证 → 写入凭据」之间隔着数秒 await，
/// 并发跑会让同一个 Key 通过两次去重检查、被上号两次。
static POLL_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 上游返回的单个 Key
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteKeyItem {
    pub key: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
}

/// 上游响应
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteKeysResponse {
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub active: u32,
    #[serde(default)]
    pub keys: Vec<RemoteKeyItem>,
}

/// 本次成功上号的一条明细
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardedItem {
    pub credential_id: u64,
    pub key_masked: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 一次轮询的结果（也是管理界面「立即上号」的返回体）
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PollOutcome {
    /// 是否成功拉取并解析了响应
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 响应里的 Key 总数 / active 数
    pub total: u32,
    pub active: u32,
    /// 过滤+去重后待上号的数量
    pub candidates: u32,
    /// 因已在凭据池里而跳过的数量
    pub already_in_pool: u32,
    /// 因去重表记录（已上号过 / 失败次数超限）而跳过的数量
    pub skipped_seen: u32,
    pub added: u32,
    pub skipped: u32,
    pub invalid: u32,
    /// 试运行：只拉取和比对，不实际导入
    pub dry_run: bool,
    pub summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub onboarded: Vec<OnboardedItem>,
    /// 逐条失败原因（导入失败 / 解析失败）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Key 的去重指纹：sha256 十六进制（库里只存哈希，不留第二份明文）
pub fn key_hash(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// 查去重表：返回 (outcome, attempts)
fn lookup_seen(service: &Arc<AdminService>, key: &str) -> Option<(String, u32)> {
    let store = service.store()?;
    match store.get_key_seen(&key_hash(key)) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "查询自动上号去重表失败（本轮按未处理对待）");
            None
        }
    }
}

/// 写去重表（失败仅告警）
fn record_seen(
    service: &Arc<AdminService>,
    key: &str,
    outcome: &str,
    credential_id: Option<u64>,
    note: Option<String>,
    count_attempt: bool,
) {
    let Some(store) = service.store() else {
        return;
    };
    let rec = crate::storage::KeySeenUpsert {
        key_hash: key_hash(key),
        key_masked: mask_key(key),
        outcome: outcome.to_string(),
        credential_id,
        at: chrono::Utc::now(),
        note,
        count_attempt,
    };
    if let Err(e) = store.upsert_key_seen(&rec) {
        tracing::warn!(error = %e, "写入自动上号去重表失败");
    }
}

/// 为「已在凭据池里」的 Key 补一条终态记录，仅在尚未记录时写入
/// （已有记录就别动，避免每轮把 attempts 刷成天文数字）
fn mark_seen_if_absent(service: &Arc<AdminService>, key: &str) {
    if lookup_seen(service, key).is_some() {
        return;
    }
    record_seen(
        service,
        key,
        "onboarded",
        None,
        Some("已在凭据池中（非本功能导入）".to_string()),
        false,
    );
}

/// 脱敏 Key：`ksk_abcd***mnop`
pub fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        return "***".to_string();
    }
    let prefix_end = floor_char_boundary(key, 8);
    let suffix_start = floor_char_boundary(key, key.len() - 4);
    format!("{}***{}", &key[..prefix_end], &key[suffix_start..])
}

fn truncate_body(s: String) -> String {
    if s.len() <= KEY_POLL_LOG_MAX_BODY_BYTES {
        return s;
    }
    let end = floor_char_boundary(&s, KEY_POLL_LOG_MAX_BODY_BYTES);
    let total = s.len();
    format!(
        "{}\n…（已截断，原文 {} 字节，仅保留前 {} 字节）",
        &s[..end],
        total,
        end
    )
}

/// 执行一次轮询。`trigger_kind`: `auto` | `manual`；`dry_run` 只拉取比对不导入。
///
/// 不返回 Err：拉取失败也算一次「结果」，落库并把原因放进 `errors`，
/// 这样后台任务不会因为一次网络抖动中断，管理界面也能看到失败记录。
pub async fn poll_once(
    service: &Arc<AdminService>,
    trigger_kind: &str,
    dry_run: bool,
) -> PollOutcome {
    let (api_url, api_key, priority, only_active, log_enabled, retry_invalid_max, tls_backend) = {
        let cfg = service.config().read();
        (
            cfg.key_poll.api_url.trim().to_string(),
            cfg.key_poll.api_key.trim().to_string(),
            cfg.key_poll.priority,
            cfg.key_poll.only_active,
            cfg.key_poll.log_enabled,
            cfg.key_poll.retry_invalid_max,
            cfg.tls_backend,
        )
    };

    let mut outcome = PollOutcome {
        dry_run,
        ..Default::default()
    };

    // 同一时刻只允许一次轮询在跑（见 POLL_GUARD 注释）
    let _guard = match POLL_GUARD.try_lock() {
        Ok(g) => g,
        Err(_) => {
            outcome.summary = "已有一次上号正在进行，本次跳过".to_string();
            tracing::info!(trigger = trigger_kind, "自动上号：已有轮询在跑，跳过本次");
            return outcome;
        }
    };

    if api_url.is_empty() || api_key.is_empty() {
        outcome.summary = "未配置接口地址或密钥".to_string();
        outcome.errors.push(outcome.summary.clone());
        record_poll_log(service, trigger_kind, &outcome, None, log_enabled);
        return outcome;
    }

    // 拉取（直连，不走凭据代理：这是自家发卡站，与上游 Kiro 无关）
    let client = match crate::http_client::build_client(None, FETCH_TIMEOUT_SECS, tls_backend) {
        Ok(c) => c,
        Err(e) => {
            outcome.summary = format!("构建 HTTP 客户端失败: {e}");
            outcome.errors.push(outcome.summary.clone());
            record_poll_log(service, trigger_kind, &outcome, None, log_enabled);
            return outcome;
        }
    };

    let resp = client
        .get(&api_url)
        .header("X-API-Key", &api_key)
        .send()
        .await;

    let (status, body) = match resp {
        Ok(r) => {
            let status = r.status();
            outcome.http_status = Some(status.as_u16());
            match r.text().await {
                Ok(b) => (status, b),
                Err(e) => {
                    outcome.summary = format!("读取响应失败: {e}");
                    outcome.errors.push(outcome.summary.clone());
                    record_poll_log(service, trigger_kind, &outcome, None, log_enabled);
                    return outcome;
                }
            }
        }
        Err(e) => {
            outcome.summary = format!("请求失败: {e}");
            outcome.errors.push(outcome.summary.clone());
            record_poll_log(service, trigger_kind, &outcome, None, log_enabled);
            return outcome;
        }
    };

    if !status.is_success() {
        outcome.summary = format!("上游返回 {}", status.as_u16());
        outcome.errors.push(outcome.summary.clone());
        record_poll_log(service, trigger_kind, &outcome, Some(&body), log_enabled);
        return outcome;
    }

    let parsed: RemoteKeysResponse = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            outcome.summary = format!("响应 JSON 解析失败: {e}");
            outcome.errors.push(outcome.summary.clone());
            record_poll_log(service, trigger_kind, &outcome, Some(&body), log_enabled);
            return outcome;
        }
    };

    outcome.ok = true;
    outcome.total = parsed.count.max(parsed.keys.len() as u32);
    outcome.active = parsed.active;

    // 过滤 + 去重 + 剔除本地已有 + 剔除处理过的（持久去重表）
    let mut seen_in_batch = std::collections::HashSet::new();
    let mut candidates: Vec<RemoteKeyItem> = Vec::new();
    for item in parsed.keys {
        let key = item.key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        if only_active
            && !item
                .status
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("active"))
                .unwrap_or(false)
        {
            continue;
        }
        if !seen_in_batch.insert(key.clone()) {
            continue;
        }
        if service.token_manager().has_kiro_api_key(&key) {
            outcome.already_in_pool += 1;
            // 补一条终态记录：手工导入的 Key 也纳入去重，
            // 这样它对应的凭据日后被删掉也不会被自动重新上号
            mark_seen_if_absent(service, &key);
            continue;
        }
        // 持久去重：上过号的永久跳过；失败过的按重试上限跳过
        // （凭据被删掉后再次拉到同一 Key 也不会重复上号）
        match lookup_seen(service, &key) {
            Some((outcome_kind, _)) if outcome_kind == "onboarded" => {
                outcome.skipped_seen += 1;
                continue;
            }
            // 只有「凭据本身无效」才按次数拉黑；retrying/skipped 永远还有下一次
            Some((kind, attempts))
                if kind == "invalid" && retry_invalid_max > 0 && attempts >= retry_invalid_max =>
            {
                outcome.skipped_seen += 1;
                continue;
            }
            _ => {}
        }
        candidates.push(RemoteKeyItem { key, ..item });
    }
    outcome.candidates = candidates.len() as u32;

    if candidates.is_empty() {
        outcome.summary = format!(
            "无新 Key（上游 {} 个，active {}，已在池 {}，已处理跳过 {}）",
            outcome.total, outcome.active, outcome.already_in_pool, outcome.skipped_seen
        );
        record_poll_log(service, trigger_kind, &outcome, Some(&body), log_enabled);
        return outcome;
    }

    if dry_run {
        outcome.summary = format!("试运行：发现 {} 个新 Key（未导入）", outcome.candidates);
        record_poll_log(service, trigger_kind, &outcome, Some(&body), log_enabled);
        return outcome;
    }

    tracing::info!(
        trigger = trigger_kind,
        candidates = outcome.candidates,
        "自动上号：发现新 Key，开始导入"
    );

    // 走批量导入管线：逐条验证 → 代理入池/自动分配 → force_enable 直接启用
    let items: Vec<TokenJsonItem> = candidates
        .iter()
        .map(|c| TokenJsonItem {
            provider: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some(c.key.clone()),
            endpoint: None,
            priority,
            region: None,
            api_region: None,
            machine_id: None,
            email: None,
            proxy: None,
        })
        .collect();

    let import = service
        .import_token_json_with_options(
            ImportTokenJsonRequest {
                dry_run: false,
                items: ImportItems::Multiple(items),
            },
            true,
        )
        .await;

    outcome.added = import.summary.added as u32;
    outcome.skipped = import.summary.skipped as u32;
    outcome.invalid = import.summary.invalid as u32;

    // 逐条结果 → 上号记录（按 index 对回候选，拿 order_id）
    let snapshot = service.token_manager().snapshot();
    for result in &import.items {
        let candidate = candidates.get(result.index);
        match result.action {
            ImportAction::Added => {
                let Some(credential_id) = result.credential_id else {
                    continue;
                };
                let entry = snapshot.entries.iter().find(|e| e.id == credential_id);
                let proxy_id = entry.and_then(|e| e.proxy_slot_id.clone()).or_else(|| {
                    service
                        .proxy_pool_ref()
                        .and_then(|p| p.find_binding_for(credential_id))
                });
                // 代理 URL 可能含 user:pass，返回给前端前脱敏（与列表接口口径一致）
                let proxy_url = proxy_id.as_deref().and_then(|id| {
                    service
                        .proxy_pool_ref()
                        .and_then(|p| p.get(id))
                        .map(|e| crate::common::redact::mask_url_userinfo(&e.url))
                });
                let enabled = entry.map(|e| !e.disabled).unwrap_or(false);
                let onboarded = OnboardedItem {
                    credential_id,
                    key_masked: candidate
                        .map(|c| mask_key(&c.key))
                        .unwrap_or_else(|| result.fingerprint.clone()),
                    order_id: candidate.and_then(|c| c.order_id.clone()),
                    proxy_id,
                    proxy_url,
                    enabled,
                    note: result.reason.clone(),
                };
                record_onboard_log(service, trigger_kind, &onboarded);
                // 终态：这个 Key 以后即使凭据被删也不再重复上号
                if let Some(c) = candidate {
                    record_seen(
                        service,
                        &c.key,
                        "onboarded",
                        Some(credential_id),
                        onboarded.note.clone(),
                        true,
                    );
                }
                outcome.onboarded.push(onboarded);
            }
            ImportAction::Invalid => {
                let masked = candidate
                    .map(|c| mask_key(&c.key))
                    .unwrap_or_else(|| result.fingerprint.clone());
                let reason = result.reason.clone();
                // 环境类失败（无可用代理槽、上游网络/限流）记成 retrying 且不计次数，
                // 避免代理池临时耗尽这类全局故障把一批好 Key 永久拉黑
                if let Some(c) = candidate {
                    let (kind, count) = if result.retryable {
                        ("retrying", false)
                    } else {
                        ("invalid", true)
                    };
                    record_seen(service, &c.key, kind, None, reason.clone(), count);
                }
                outcome.errors.push(format!(
                    "{}: {}",
                    masked,
                    reason.as_deref().unwrap_or("未知原因")
                ));
            }
            ImportAction::Skipped => {
                // 上游管线判定「凭据已存在」——也记一笔，避免下轮再走一遍验证
                if let Some(c) = candidate {
                    record_seen(
                        service,
                        &c.key,
                        "skipped",
                        None,
                        result.reason.clone(),
                        false,
                    );
                }
            }
        }
    }

    outcome.summary = format!(
        "上游 {} 个（active {}），新增 {}，跳过 {}，失败 {}{}",
        outcome.total,
        outcome.active,
        outcome.added,
        outcome.skipped,
        outcome.invalid,
        if outcome.already_in_pool + outcome.skipped_seen > 0 {
            format!(
                "（已在池 {}，已处理跳过 {}）",
                outcome.already_in_pool, outcome.skipped_seen
            )
        } else {
            String::new()
        }
    );
    tracing::info!(
        trigger = trigger_kind,
        added = outcome.added,
        invalid = outcome.invalid,
        "自动上号：导入完成"
    );
    record_poll_log(service, trigger_kind, &outcome, Some(&body), log_enabled);
    outcome
}

/// 落一条轮询记录（失败仅告警，不影响主流程）
fn record_poll_log(
    service: &Arc<AdminService>,
    trigger_kind: &str,
    outcome: &PollOutcome,
    body: Option<&str>,
    log_enabled: bool,
) {
    let Some(store) = service.store() else {
        return;
    };
    // 试运行不写库（避免手动点「试运行」把记录刷满）
    if outcome.dry_run {
        return;
    }
    let insert = KeyPollLogInsert {
        at: chrono::Utc::now(),
        trigger_kind: trigger_kind.to_string(),
        ok: outcome.ok,
        http_status: outcome.http_status,
        total: outcome.total,
        active: outcome.active,
        added: outcome.added,
        skipped: outcome.skipped,
        invalid: outcome.invalid,
        summary: if outcome.summary.is_empty() {
            "（无）".to_string()
        } else {
            outcome.summary.clone()
        },
        response_body: if log_enabled {
            body.map(|b| truncate_body(b.to_string()))
        } else {
            None
        },
    };
    if let Err(e) = store.insert_key_poll_log(&insert) {
        tracing::warn!(error = %e, "写入自动上号轮询记录失败");
    }
}

/// 落一条上号成功记录
fn record_onboard_log(service: &Arc<AdminService>, trigger_kind: &str, item: &OnboardedItem) {
    let Some(store) = service.store() else {
        return;
    };
    let insert = KeyOnboardLogInsert {
        at: chrono::Utc::now(),
        credential_id: item.credential_id,
        key_masked: item.key_masked.clone(),
        order_id: item.order_id.clone(),
        trigger_kind: trigger_kind.to_string(),
        proxy_id: item.proxy_id.clone(),
        proxy_url: item.proxy_url.clone(),
        enabled: item.enabled,
        note: item.note.clone(),
    };
    if let Err(e) = store.insert_key_onboard_log(&insert) {
        tracing::warn!(error = %e, "写入上号记录失败");
    }
}

/// 启动后台轮询任务
///
/// 每 10 秒 tick 一次，按当前配置的间隔判断是否该轮询——间隔/开关在管理界面改完
/// 下一个 tick 即生效，无需重启。启动后先等一个 tick 再首次轮询，避开启动期拥挤。
pub fn spawn_poll_loop(service: Arc<AdminService>) {
    tokio::spawn(async move {
        const TICK_SECS: u64 = 10;
        let mut ticker = tokio::time::interval(Duration::from_secs(TICK_SECS));
        ticker.tick().await; // 跳过立即触发的首个 tick
        let mut last_poll: Option<tokio::time::Instant> = None;
        loop {
            ticker.tick().await;
            let (enabled, interval_secs, has_key) = {
                let cfg = service.config().read();
                (
                    cfg.key_poll.enabled,
                    cfg.key_poll.interval_secs.max(KEY_POLL_MIN_INTERVAL_SECS),
                    !cfg.key_poll.api_key.trim().is_empty(),
                )
            };
            if !enabled || !has_key {
                continue;
            }
            let due = last_poll
                .map(|t| t.elapsed() >= Duration::from_secs(interval_secs))
                .unwrap_or(true);
            if !due {
                continue;
            }
            last_poll = Some(tokio::time::Instant::now());
            let outcome = poll_once(&service, "auto", false).await;
            if !outcome.ok {
                tracing::warn!("自动上号轮询失败: {}", outcome.summary);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("ksk_abcdefghijklmnop"), "ksk_abcd***mnop");
        assert_eq!(mask_key("short"), "***");
    }

    #[test]
    fn test_parse_response_shape() {
        // 用户给出的真实响应形态
        let raw = r#"{"count":5,"active":3,"keys":[
            {"key":"ksk_aaa","status":"active","order_id":"o1","created_at":"2026-07-01"},
            {"key":"ksk_bbb","status":"expired","order_id":"o2","created_at":"2026-07-02"}
        ]}"#;
        let parsed: RemoteKeysResponse = serde_json::from_str(raw).unwrap();
        assert_eq!((parsed.count, parsed.active), (5, 3));
        assert_eq!(parsed.keys.len(), 2);
        assert_eq!(parsed.keys[0].key, "ksk_aaa");
        assert_eq!(parsed.keys[0].status.as_deref(), Some("active"));
        assert_eq!(parsed.keys[0].order_id.as_deref(), Some("o1"));
    }

    #[test]
    fn test_parse_empty_and_extra_fields() {
        // 空列表 + 未知字段（suspect）不应导致解析失败
        let raw = r#"{"active":0,"count":0,"keys":[],"suspect":0}"#;
        let parsed: RemoteKeysResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.keys.len(), 0);
        assert_eq!(parsed.count, 0);
        // keys 里缺 status / order_id 也能解析
        let raw2 = r#"{"keys":[{"key":"ksk_x"}]}"#;
        let p2: RemoteKeysResponse = serde_json::from_str(raw2).unwrap();
        assert_eq!(p2.keys[0].status, None);
        assert_eq!(p2.keys[0].order_id, None);
    }

    #[test]
    fn test_truncate_body_marks_truncation() {
        let long = "x".repeat(KEY_POLL_LOG_MAX_BODY_BYTES + 100);
        let out = truncate_body(long);
        assert!(out.contains("已截断"));
        assert!(out.len() < KEY_POLL_LOG_MAX_BODY_BYTES + 200);
        // 未超限时原样返回
        assert_eq!(truncate_body("short".to_string()), "short");
    }
}
