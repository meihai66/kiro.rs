//! 提醒推送模块（ogpush）
//!
//! 阈值告警：可用凭据数 / 预计可用时长低于配置阈值时，调用 ogpush 推送接口通知。
//! 同一轮检查两个条件同时触发只合并推送一条；相邻两次推送固定间隔至少 30 分钟。

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::{Config, PushNotificationConfig};

/// 相邻两次告警推送的最小间隔（硬编码 30 分钟）
const PUSH_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 阈值检查周期
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// 调用 ogpush 推送接口发送一条消息，返回实际推送的用户数。
///
/// 与总开关 `enabled` 无关（测试推送时也可用），只要求密钥与收件人已配置。
pub async fn send_push(
    cfg: &PushNotificationConfig,
    title: &str,
    body: &str,
) -> anyhow::Result<u64> {
    let api_key = cfg.api_key.trim();
    if api_key.is_empty() {
        anyhow::bail!("推送 API Key 未配置");
    }
    if !cfg.has_recipients() {
        anyhow::bail!("未配置任何收件人（用户组 / 用户 id / 用户名）");
    }
    let url = match cfg.api_url.trim() {
        "" => "https://ogpush.ogog.dev/api/push",
        u => u,
    };

    let mut payload = serde_json::json!({
        "title": title,
        "body": body,
        "priority": if cfg.priority == "urgent" { "urgent" } else { "normal" },
    });
    if !cfg.group_ids.is_empty() {
        payload["group_ids"] = serde_json::json!(cfg.group_ids);
    }
    if !cfg.user_ids.is_empty() {
        payload["user_ids"] = serde_json::json!(cfg.user_ids);
    }
    if !cfg.usernames.is_empty() {
        payload["usernames"] = serde_json::json!(cfg.usernames);
    }

    // 推送频率极低（≥30 分钟一次），每次现建 client 即可；
    // no_proxy：推送目标是公网通知服务，不跟随系统代理环境变量，避免被慢代理拖挂。
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()?;
    let resp = client
        .post(url)
        .header("X-API-Key", api_key)
        .json(&payload)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("推送接口返回 {}: {}", status, text);
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    Ok(v.get("pushed").and_then(|p| p.as_u64()).unwrap_or(0))
}

/// 启动阈值监控后台任务：每分钟检查一次
/// - 可用凭据数（未禁用且不在冷却）< min_available_credentials
/// - 预计可用时长（剩余点数 / 最近 N 分钟消耗速率）< min_remaining_minutes
///
/// 任一触发即推送；两项同时触发合并为一条。推送成功后 30 分钟内不再推送；
/// 推送失败不占用冷却窗口，下一轮继续尝试。
pub fn spawn_alert_monitor(config: Arc<RwLock<Config>>, token_manager: Arc<MultiTokenManager>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(CHECK_INTERVAL);
        ticker.tick().await; // 跳过首个立即 tick，启动后先让余额缓存等 settle
        let mut last_push_at: Option<Instant> = None;
        loop {
            ticker.tick().await;
            let cfg = config.read().push_notification.clone();
            if !cfg.enabled
                || (cfg.min_available_credentials == 0 && cfg.min_remaining_minutes == 0)
            {
                continue;
            }
            if let Some(t) = last_push_at
                && t.elapsed() < PUSH_MIN_INTERVAL
            {
                continue;
            }

            let mut alerts: Vec<String> = Vec::new();

            if cfg.min_available_credentials > 0 {
                let snapshot = token_manager.snapshot();
                let available = snapshot
                    .entries
                    .iter()
                    .filter(|e| !e.disabled && e.cooldown_remaining_secs.is_none())
                    .count();
                if (available as u32) < cfg.min_available_credentials {
                    alerts.push(format!(
                        "可用凭据仅剩 {} 个（阈值 {}）",
                        available, cfg.min_available_credentials
                    ));
                }
            }

            if cfg.min_remaining_minutes > 0 {
                let window = cfg.credit_window_minutes.clamp(1, 60) as u64;
                let recent = token_manager.recent_credit_usage(Duration::from_secs(window * 60));
                let per_minute = recent / window as f64;
                // 窗口内无消耗时无法估算，视为不触发（没流量也就烧不掉点数）
                if per_minute > 0.0 {
                    let (remaining, _, _) = token_manager.total_remaining_balance();
                    let est_minutes = remaining / per_minute;
                    if est_minutes < cfg.min_remaining_minutes as f64 {
                        alerts.push(format!(
                            "预计可用时长约 {:.0} 分钟（阈值 {} 分钟；剩余 {:.1} 点，最近 {} 分钟速率 {:.3} 点/分）",
                            est_minutes,
                            cfg.min_remaining_minutes,
                            remaining,
                            window,
                            per_minute
                        ));
                    }
                }
            }

            if alerts.is_empty() {
                continue;
            }
            let body = alerts.join("；");
            match send_push(&cfg, "kiro-rs 阈值告警", &body).await {
                Ok(pushed) => {
                    last_push_at = Some(Instant::now());
                    tracing::info!(pushed, body = %body, "阈值告警推送已发送");
                }
                Err(e) => {
                    tracing::warn!("阈值告警推送失败（下一轮重试）: {}", e);
                }
            }
        }
    });
}
