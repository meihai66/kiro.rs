//! Webhook API 模块
//!
//! 供外部系统自动推送 Kiro API Key（`ksk_*`）入池：
//! `POST /api/webhook/import-keys`
//!
//! - 认证使用独立的 `webhookApiKey`（与 Admin API Key 分离，便于给自动化系统单独发密钥）
//! - Key 行格式与管理界面批量导入一致：`ksk_xxx` 或 `ksk_xxx|host:port:user:pass`
//!   （也支持 `ksk_xxx|host:port` / `ksk_xxx|scheme://user:pass@host:port`）
//! - 复用 import-token-json 管线：逐条验证、去重、内嵌代理自动入池并强制绑定
//! - 导入成功即直接启用（忽略「导入默认禁用」配置）；绑定代理失败的凭据仍保持禁用
//!
//! 开关与密钥都在请求时从共享 Config 读取，管理界面改完即时生效（无需重启）；
//! 每次接收会把原始请求体与返回体落 `webhook_logs` 表供管理界面查阅。

use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::admin::AdminService;
use crate::admin::types::{
    AdminErrorResponse, ImportItems, ImportTokenJsonRequest, ImportTokenJsonResponse,
    TokenJsonItem, TokenJsonProxyItem,
};
use crate::common::auth;
use crate::model::config::Config;
use crate::storage::{Store, WebhookLogInsert};

/// 落库的原始请求体上限（超出部分截断并标注），防止推送方灌大包把库撑爆
pub const WEBHOOK_LOG_MAX_BODY_BYTES: usize = 64 * 1024;

/// Webhook API 共享状态
#[derive(Clone)]
pub struct WebhookState {
    /// 共享配置（开关 / 密钥请求时读取，支持管理界面热更新）
    pub config: Arc<RwLock<Config>>,
    /// 复用 Admin 服务的导入管线
    pub service: Arc<AdminService>,
    /// 接收日志存储（None 时不记录）
    pub store: Option<Arc<Store>>,
}

/// Webhook 认证中间件：先判开关，再常量时间比对密钥
/// （x-api-key / Authorization: Bearer）
async fn webhook_auth_middleware(
    State(state): State<WebhookState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let (enabled, configured_key) = {
        let cfg = state.config.read();
        (
            cfg.webhook_enabled,
            cfg.webhook_api_key
                .as_ref()
                .map(|k| k.trim().to_string())
                .unwrap_or_default(),
        )
    };

    if !enabled {
        let error = AdminErrorResponse::invalid_request("Webhook 接口已关闭");
        return (StatusCode::FORBIDDEN, Json(error)).into_response();
    }
    if configured_key.is_empty() {
        let error = AdminErrorResponse::invalid_request("Webhook 密钥未配置");
        return (StatusCode::FORBIDDEN, Json(error)).into_response();
    }

    match auth::extract_api_key(&request) {
        Some(key) if auth::constant_time_eq(&key, &configured_key) => next.run(request).await,
        _ => {
            let error = AdminErrorResponse::authentication_error();
            (StatusCode::UNAUTHORIZED, Json(error)).into_response()
        }
    }
}

/// Key 列表：接受字符串（按行拆分）或字符串数组，方便不同推送方对接
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum KeysInput {
    Text(String),
    List(Vec<String>),
}

impl KeysInput {
    fn into_lines(self) -> Vec<String> {
        let raw = match self {
            KeysInput::Text(s) => s.lines().map(str::to_string).collect(),
            KeysInput::List(v) => v,
        };
        raw.into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Webhook 导入请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookImportKeysRequest {
    /// Key 行列表（数组或多行字符串），每行 `ksk_xxx[|proxy]`
    pub keys: KeysInput,
    /// 凭据优先级（数字越小越优先），默认 10
    #[serde(default)]
    pub priority: Option<u32>,
    /// Region（可选，留空用全局配置）
    #[serde(default)]
    pub region: Option<String>,
    /// API Region（可选覆盖）
    #[serde(default)]
    pub api_region: Option<String>,
    /// 凭据级端点（`ide` / `cli`，可选）
    #[serde(default)]
    pub endpoint: Option<String>,
    /// `host:port[:user:pass]` 列表格式代理所用协议，默认 http
    #[serde(default = "default_proxy_scheme")]
    pub proxy_scheme: String,
}

fn default_proxy_scheme() -> String {
    "http".to_string()
}

/// 行解析失败明细
#[derive(Debug, Serialize)]
pub struct LineError {
    /// 出错行（Key 已脱敏）
    pub line: String,
    pub error: String,
}

/// Webhook 导入响应：导入管线结果 + 行解析错误
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookImportKeysResponse {
    /// 收到的非空行数（含解析失败的行）
    pub received: usize,
    #[serde(flatten)]
    pub import: ImportTokenJsonResponse,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub line_errors: Vec<LineError>,
}

/// 解析一行 `ksk_xxx[|proxy]`，返回 (key, 可选代理 URL)
///
/// 代理段三种形态（与管理界面批量导入一致）：
/// - `scheme://[user:pass@]host:port` —— 原样使用
/// - `host:port` —— 补 scheme
/// - `host:port:user:pass` —— user/pass 百分号编码后拼入 URL（pass 可含 `:`）
fn parse_key_line(line: &str, scheme: &str) -> Result<(String, Option<String>), String> {
    let (key, proxy_raw) = match line.split_once('|') {
        Some((k, p)) => (k.trim(), p.trim()),
        None => (line.trim(), ""),
    };
    if key.is_empty() {
        return Err("缺少 API Key".to_string());
    }
    if proxy_raw.is_empty() {
        return Ok((key.to_string(), None));
    }
    if proxy_raw.contains("://") {
        return Ok((key.to_string(), Some(proxy_raw.to_string())));
    }
    let segs: Vec<&str> = proxy_raw.split(':').collect();
    let host = segs.first().copied().unwrap_or_default();
    let port: u16 = segs
        .get(1)
        .and_then(|p| p.parse().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| {
            format!("代理格式错误（期望 host:port:user:pass 或 host:port）: {proxy_raw}")
        })?;
    if host.is_empty() || (segs.len() != 2 && segs.len() < 4) {
        return Err(format!(
            "代理格式错误（期望 host:port:user:pass 或 host:port）: {proxy_raw}"
        ));
    }
    let user = segs.get(2).copied().unwrap_or_default();
    let pass = segs.get(3..).map(|s| s.join(":")).unwrap_or_default();
    let auth = if user.is_empty() && pass.is_empty() {
        String::new()
    } else {
        format!(
            "{}:{}@",
            urlencoding::encode(user),
            urlencoding::encode(&pass)
        )
    };
    Ok((
        key.to_string(),
        Some(format!("{scheme}://{auth}{host}:{port}")),
    ))
}

/// 脱敏 Key（行错误回显 / 日志用）
fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        return "***".to_string();
    }
    let prefix_end = crate::common::utf8::floor_char_boundary(key, 8);
    let suffix_start = crate::common::utf8::floor_char_boundary(key, key.len() - 4);
    format!("{}***{}", &key[..prefix_end], &key[suffix_start..])
}

/// `POST /import-keys` - 接收推送的 Key，绑定代理并直接启用入池
///
/// 用 `Bytes` 而非 `Json<T>` 提取：请求体先原样留存再解析，
/// 这样 JSON 格式错误的推送也能在管理界面看到收到的原文。
async fn import_keys(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw_body = String::from_utf8_lossy(&body).to_string();
    let source_ip = extract_source_ip(&headers);

    let payload: WebhookImportKeysRequest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("请求体 JSON 解析失败: {e}");
            let error = AdminErrorResponse::invalid_request(msg.clone());
            let resp_body = serde_json::to_string(&error).unwrap_or_default();
            record_log(
                &state,
                RecordLogArgs {
                    source_ip,
                    status_code: StatusCode::BAD_REQUEST.as_u16(),
                    received: 0,
                    added: 0,
                    skipped: 0,
                    invalid: 0,
                    summary: msg,
                    headers: &headers,
                    raw_body,
                    response_body: resp_body,
                },
            );
            return (StatusCode::BAD_REQUEST, Json(error)).into_response();
        }
    };

    let scheme = payload.proxy_scheme.trim().to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h") {
        let msg = format!("不支持的代理协议: {scheme}");
        let error = AdminErrorResponse::invalid_request(msg.clone());
        let resp_body = serde_json::to_string(&error).unwrap_or_default();
        record_log(
            &state,
            RecordLogArgs {
                source_ip,
                status_code: StatusCode::BAD_REQUEST.as_u16(),
                received: 0,
                added: 0,
                skipped: 0,
                invalid: 0,
                summary: msg,
                headers: &headers,
                raw_body,
                response_body: resp_body,
            },
        );
        return (StatusCode::BAD_REQUEST, Json(error)).into_response();
    }

    let lines = payload.keys.into_lines();
    let received = lines.len();
    let mut line_errors = Vec::new();
    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in &lines {
        match parse_key_line(line, &scheme) {
            // 按 Key 去重，保留首次出现（与管理界面批量导入一致）
            Ok((key, proxy)) => {
                if seen.insert(key.clone()) {
                    entries.push((key, proxy));
                }
            }
            Err(e) => {
                let masked = match line.split_once('|') {
                    Some((k, p)) => format!("{}|{}", mask_key(k.trim()), p.trim()),
                    None => mask_key(line),
                };
                line_errors.push(LineError {
                    line: masked,
                    error: e,
                });
            }
        }
    }

    if entries.is_empty() && line_errors.is_empty() {
        let error = AdminErrorResponse::invalid_request("keys 为空");
        let resp_body = serde_json::to_string(&error).unwrap_or_default();
        record_log(
            &state,
            RecordLogArgs {
                source_ip,
                status_code: StatusCode::BAD_REQUEST.as_u16(),
                received: 0,
                added: 0,
                skipped: 0,
                invalid: 0,
                summary: "keys 为空".to_string(),
                headers: &headers,
                raw_body,
                response_body: resp_body,
            },
        );
        return (StatusCode::BAD_REQUEST, Json(error)).into_response();
    }

    let priority = payload.priority.unwrap_or(10);
    let items: Vec<TokenJsonItem> = entries
        .into_iter()
        .map(|(key, proxy_url)| TokenJsonItem {
            provider: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some(key),
            endpoint: payload.endpoint.clone(),
            priority,
            region: payload.region.clone(),
            api_region: payload.api_region.clone(),
            machine_id: None,
            email: None,
            proxy: proxy_url.map(|url| TokenJsonProxyItem {
                url,
                proxy_type: None,
                expires_at: None,
                label: Some("webhook".to_string()),
            }),
        })
        .collect();

    let parsed_ok = items.len();
    tracing::info!(
        received,
        parsed = parsed_ok,
        line_errors = line_errors.len(),
        "webhook: 收到 Key 推送，开始导入"
    );

    let import = state
        .service
        .import_token_json_with_options(
            ImportTokenJsonRequest {
                dry_run: false,
                items: ImportItems::Multiple(items),
            },
            true,
        )
        .await;

    tracing::info!(
        added = import.summary.added,
        skipped = import.summary.skipped,
        invalid = import.summary.invalid,
        "webhook: 导入完成"
    );

    let summary = format!(
        "收到 {} 行：新增 {}，跳过 {}，失败 {}{}",
        received,
        import.summary.added,
        import.summary.skipped,
        import.summary.invalid,
        if line_errors.is_empty() {
            String::new()
        } else {
            format!("，格式错误 {}", line_errors.len())
        }
    );
    let response = WebhookImportKeysResponse {
        received,
        import,
        line_errors,
    };
    let resp_body = serde_json::to_string(&response).unwrap_or_default();
    record_log(
        &state,
        RecordLogArgs {
            source_ip,
            status_code: StatusCode::OK.as_u16(),
            received: received as u32,
            added: response.import.summary.added as u32,
            skipped: response.import.summary.skipped as u32,
            invalid: response.import.summary.invalid as u32,
            summary,
            headers: &headers,
            raw_body,
            response_body: resp_body,
        },
    );

    Json(response).into_response()
}

/// 取来源 IP：优先 `X-Forwarded-For` 首段，其次 `X-Real-IP`
/// （服务未启用 ConnectInfo，直连场景取不到对端地址时返回 None）
fn extract_source_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

/// 采集用于落库的请求头（只留与排障相关的几个，避免把认证密钥写进库）
fn collect_headers(headers: &HeaderMap) -> String {
    const KEEP: [&str; 5] = [
        "content-type",
        "content-length",
        "user-agent",
        "x-forwarded-for",
        "x-real-ip",
    ];
    let mut lines: Vec<String> = KEEP
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{name}: {v}"))
        })
        .collect();
    // 认证头只记录「是否携带」，不落具体值
    if headers.contains_key("x-api-key") {
        lines.push("x-api-key: <present>".to_string());
    }
    if headers.contains_key("authorization") {
        lines.push("authorization: <present>".to_string());
    }
    lines.join("\n")
}

/// 按字节上限截断文本（多字节安全），超出时追加截断标注
fn truncate_body(s: String) -> String {
    if s.len() <= WEBHOOK_LOG_MAX_BODY_BYTES {
        return s;
    }
    let end = crate::common::utf8::floor_char_boundary(&s, WEBHOOK_LOG_MAX_BODY_BYTES);
    let total = s.len();
    format!(
        "{}\n…（已截断，原文 {} 字节，仅保留前 {} 字节）",
        &s[..end],
        total,
        end
    )
}

/// `record_log` 的参数集合（字段较多，独立结构体避免长参数列表）
struct RecordLogArgs<'a> {
    source_ip: Option<String>,
    status_code: u16,
    received: u32,
    added: u32,
    skipped: u32,
    invalid: u32,
    summary: String,
    headers: &'a HeaderMap,
    raw_body: String,
    response_body: String,
}

/// 记录一次接收（同步写入，单条 insert 开销可忽略；失败仅告警不影响响应）
fn record_log(state: &WebhookState, args: RecordLogArgs<'_>) {
    if !state.config.read().webhook_log_enabled {
        return;
    }
    let Some(store) = state.store.as_ref() else {
        return;
    };
    let insert = WebhookLogInsert {
        at: chrono::Utc::now(),
        source_ip: args.source_ip,
        status_code: args.status_code,
        received: args.received,
        added: args.added,
        skipped: args.skipped,
        invalid: args.invalid,
        summary: args.summary,
        request_headers: Some(collect_headers(args.headers)).filter(|s| !s.is_empty()),
        request_body: Some(truncate_body(args.raw_body)),
        response_body: Some(truncate_body(args.response_body)),
    };
    if let Err(e) = store.insert_webhook_log(&insert) {
        tracing::warn!(error = %e, "写入 Webhook 接收日志失败");
    }
}

/// 创建 Webhook 路由
pub fn create_webhook_router(state: WebhookState) -> Router {
    Router::new()
        .route("/import-keys", post(import_keys))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            webhook_auth_middleware,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plain_key() {
        assert_eq!(
            parse_key_line("ksk_abc123", "http").unwrap(),
            ("ksk_abc123".to_string(), None)
        );
    }

    #[test]
    fn test_parse_key_with_host_port() {
        assert_eq!(
            parse_key_line("ksk_abc|1.2.3.4:8080", "socks5").unwrap(),
            (
                "ksk_abc".to_string(),
                Some("socks5://1.2.3.4:8080".to_string())
            )
        );
    }

    #[test]
    fn test_parse_key_with_host_port_user_pass() {
        assert_eq!(
            parse_key_line("ksk_abc|1.2.3.4:8080:user:p@ss", "http").unwrap(),
            (
                "ksk_abc".to_string(),
                Some("http://user:p%40ss@1.2.3.4:8080".to_string())
            )
        );
    }

    #[test]
    fn test_parse_pass_with_colon() {
        // pass 含 ':'：第 4 段之后全部并入 pass
        assert_eq!(
            parse_key_line("ksk_abc|1.2.3.4:8080:u:a:b", "http").unwrap(),
            (
                "ksk_abc".to_string(),
                Some("http://u:a%3Ab@1.2.3.4:8080".to_string())
            )
        );
    }

    #[test]
    fn test_parse_full_url_proxy() {
        assert_eq!(
            parse_key_line("ksk_abc|socks5://u:p@h:1080", "http").unwrap(),
            (
                "ksk_abc".to_string(),
                Some("socks5://u:p@h:1080".to_string())
            )
        );
    }

    #[test]
    fn test_parse_errors() {
        assert!(parse_key_line("|1.2.3.4:8080", "http").is_err()); // 缺 key
        assert!(parse_key_line("ksk_abc|1.2.3.4", "http").is_err()); // 缺 port
        assert!(parse_key_line("ksk_abc|1.2.3.4:abc", "http").is_err()); // port 非数字
        assert!(parse_key_line("ksk_abc|1.2.3.4:8080:user", "http").is_err()); // 3 段
        assert!(parse_key_line("ksk_abc|:8080", "http").is_err()); // 缺 host
    }

    #[test]
    fn test_keys_input_text_and_list() {
        let text = KeysInput::Text("ksk_a\n\n  ksk_b  \n".to_string());
        assert_eq!(text.into_lines(), vec!["ksk_a", "ksk_b"]);
        let list = KeysInput::List(vec!["ksk_a".into(), " ".into(), "ksk_b".into()]);
        assert_eq!(list.into_lines(), vec!["ksk_a", "ksk_b"]);
    }

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("ksk_short"), "***");
        assert_eq!(mask_key("ksk_abcdefghijklmnop"), "ksk_abcd***mnop");
    }
}
