//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试

use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use reqwest::Client;
use reqwest::header::HeaderMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[cfg(not(feature = "sensitive-logs"))]
use crate::common::utf8::floor_char_boundary;
use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{
    CliEndpoint, IDE_ENDPOINT_NAME, IdeEndpoint, KiroEndpoint, RequestContext,
};
use crate::kiro::machine_id;
use crate::kiro::metrics::InFlightGuard;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{CallContext, MultiTokenManager};

/// API 调用结果
pub struct ApiCallResult {
    pub response: reqwest::Response,
    pub credential_id: u64,
}

/// MCP 调用结果
pub struct McpCallResult {
    pub response: reqwest::Response,
    pub credential_id: u64,
}

/// 上游调用尝试失败的错误包装：携带最后一次尝试的凭据 ID 与上游 HTTP 状态码。
///
/// `Display` 完全透传内部错误文本，handler 层既有的字符串分类匹配不受影响。
/// `upstream_status = Some(..)` 表示确实收到了上游的错误响应（区别于本地调度失败、
/// 网络发送失败等未到达上游的情况），handler 据此决定是否计入 API Key 级失败统计，
/// 并填充错误日志的 credential_id / upstream_status 字段。
#[derive(Debug)]
pub struct UpstreamAttemptError {
    pub credential_id: Option<u64>,
    pub upstream_status: Option<u16>,
    source: anyhow::Error,
}

impl UpstreamAttemptError {
    pub(crate) fn wrap(
        credential_id: u64,
        upstream_status: Option<u16>,
        source: anyhow::Error,
    ) -> anyhow::Error {
        anyhow::Error::new(Self {
            credential_id: Some(credential_id),
            upstream_status,
            source,
        })
    }

    /// 该错误是否发生在「已收到上游响应」之后（含 2xx 后处理失败的场景，
    /// 如读响应体失败 / MCP 200 但携带 error 负载——此时 upstream_status 为该 2xx 值）。
    /// 是 API Key 级成功/失败统计「只计上游」口径的唯一判定入口。
    pub(crate) fn reached_upstream(err: &anyhow::Error) -> bool {
        err.downcast_ref::<Self>()
            .is_some_and(|a| a.upstream_status.is_some())
    }
}

impl std::fmt::Display for UpstreamAttemptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, f)
    }
}

impl std::error::Error for UpstreamAttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let src: &(dyn std::error::Error + Send + Sync + 'static) = self.source.as_ref();
        Some(src)
    }
}

/// 每个凭据的最大重试次数（默认值；可通过 Config.max_retries_per_credential 覆盖）
const DEFAULT_MAX_RETRIES_PER_CREDENTIAL: usize = 2;

/// 总重试次数硬上限默认值（避免无限重试；可通过 Config.max_total_retries 覆盖）
const DEFAULT_MAX_TOTAL_RETRIES: usize = 3;

/// 429 冷却默认时长（无 Retry-After 时；可被 Config.rate_limit_cooldown_min_secs 覆盖）
const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: u64 = 60;

/// 429 冷却最大时长上限（可被 Config.rate_limit_cooldown_max_secs 覆盖）
const MAX_RATE_LIMIT_COOLDOWN_SECS: u64 = 300;

/// 上游容量类 429（INSUFFICIENT_MODEL_CAPACITY / high traffic）的短冷却下限。
/// 这种 429 是上游侧瞬时扩容问题，不是凭据被限流，长冷却会反复换号导致全军覆没。
/// 可被 Config.capacity_pressure_cooldown_secs 覆盖。
const CAPACITY_RATE_LIMIT_COOLDOWN_SECS: u64 = 8;

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 默认 client（无代理或全局代理）
    default_client: RwLock<Client>,
    /// 全局代理配置
    global_proxy: RwLock<Option<ProxyConfig>>,
    /// 凭据级代理 client 缓存（key: credential_id → (建缓存时的 effective_proxy, client)）
    ///
    /// 存入当时的代理配置，命中时与当前 effective_proxy 比对：
    /// 代理池轮换 / admin 换绑或解绑只更新凭据的 proxy_slot 而不经过本 provider，
    /// 若仅按 id 缓存会让凭据一直复用旧（可能已过期）代理的 client。
    client_cache: Mutex<HashMap<u64, (Option<ProxyConfig>, Client)>>,
    /// 端点实现注册表（第一阶段只注册 ide）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称
    default_endpoint: RwLock<String>,
}

impl KiroProvider {
    fn default_endpoints() -> HashMap<String, Arc<dyn KiroEndpoint>> {
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        let ide: Arc<dyn KiroEndpoint> = Arc::new(IdeEndpoint::new());
        endpoints.insert(ide.name().to_string(), ide);
        let cli: Arc<dyn KiroEndpoint> = Arc::new(CliEndpoint::new());
        endpoints.insert(cli.name().to_string(), cli);
        endpoints
    }

    /// 创建新的 KiroProvider 实例
    #[allow(dead_code)]
    pub fn new(token_manager: Arc<MultiTokenManager>) -> Self {
        Self::with_proxy(
            token_manager,
            None,
            Self::default_endpoints(),
            IDE_ENDPOINT_NAME.to_string(),
        )
    }

    /// 创建带代理配置的 KiroProvider 实例
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );

        let default_client = build_client(proxy.as_ref(), 720, token_manager.config().tls_backend)
            .expect("创建 HTTP 客户端失败");

        Self {
            token_manager,
            default_client: RwLock::new(default_client),
            global_proxy: RwLock::new(proxy),
            client_cache: Mutex::new(HashMap::new()),
            endpoints,
            default_endpoint: RwLock::new(default_endpoint),
        }
    }

    /// 热更新全局代理配置
    ///
    /// 重建 default_client 并清空 client_cache
    pub fn update_global_proxy(&self, proxy: Option<ProxyConfig>) -> anyhow::Result<()> {
        let config = self.token_manager.config();
        let new_client = build_client(proxy.as_ref(), 720, config.tls_backend)?;

        *self.global_proxy.write() = proxy;
        *self.default_client.write() = new_client;
        self.client_cache.lock().clear();

        tracing::info!("全局代理配置已热更新，client_cache 已清空");
        Ok(())
    }

    /// 热更新默认 endpoint
    pub fn update_default_endpoint(&self, default_endpoint: String) -> anyhow::Result<()> {
        if !self.endpoints.contains_key(&default_endpoint) {
            return Err(anyhow::anyhow!("未知端点: {}", default_endpoint));
        }

        *self.default_endpoint.write() = default_endpoint;
        tracing::info!("默认 endpoint 已热更新");
        Ok(())
    }

    /// 记录一次请求的用量到对应凭据（按模型累计，用于「产出价值」统计）。
    ///
    /// 仅做统计累加，失败不影响主流程。
    #[allow(clippy::too_many_arguments)]
    pub fn report_usage(
        &self,
        id: u64,
        model: &str,
        input_tokens: i32,
        output_tokens: i32,
        cache_read_tokens: i32,
        cache_write_tokens: i32,
        credit_usage: f64,
    ) {
        self.token_manager.report_usage(
            id,
            model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            credit_usage,
        );
    }

    /// 获取凭据对应的 HTTP Client
    ///
    /// 启用代理池时：必须从凭据所绑代理槽取（缺失/过期返回 Err，由调用方标 ProxyExhausted）。
    /// 未启用代理池时：使用全局代理 default_client。
    fn get_client_for_credential(&self, ctx: &CallContext) -> anyhow::Result<Client> {
        let global_proxy = self.global_proxy.read().clone();
        let effective_proxy = self
            .token_manager
            .effective_proxy_for_cred(&ctx.credentials)?;

        // 与全局代理一致 → 复用 default_client
        if effective_proxy == global_proxy {
            return Ok(self.default_client.read().clone());
        }

        {
            let cache = self.client_cache.lock();
            if let Some((cached_proxy, client)) = cache.get(&ctx.id)
                && *cached_proxy == effective_proxy
            {
                return Ok(client.clone());
            }
            // 未命中或代理已变更（轮换/换绑/解绑）→ 落到下面重建并覆盖缓存
        }

        let config = self.token_manager.config();
        let client = build_client(effective_proxy.as_ref(), 720, config.tls_backend)?;

        {
            let mut cache = self.client_cache.lock();
            cache.insert(ctx.id, (effective_proxy.clone(), client.clone()));
        }

        Ok(client)
    }

    fn endpoint_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let default_endpoint = self.default_endpoint.read();
        let name = credentials.effective_endpoint_name(Some(default_endpoint.as_str()));
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    /// 获取 token_manager 的引用
    #[allow(dead_code)]
    pub fn token_manager(&self) -> &MultiTokenManager {
        &self.token_manager
    }

    /// 后台异步刷新余额缓存（如果需要）
    fn spawn_balance_refresh(&self, id: u64) {
        // 检查缓存是否需要刷新
        if !self.token_manager.should_refresh_balance(id) {
            return;
        }
        let tm = Arc::clone(&self.token_manager);
        tokio::spawn(async move {
            match tm.get_usage_limits_for(id).await {
                Ok(resp) => {
                    // 无 usageBreakdownList 时用量会回退到 0，「数据缺失」≠「余额为零」，
                    // 不能据此用不可自愈的 InsufficientBalance 禁用凭据。保持启用，跳过判定。
                    if !resp.has_usage_data() {
                        tracing::warn!(
                            "凭据 #{} 余额响应缺少 usageBreakdownList，跳过余额判定（保持启用）",
                            id
                        );
                        return;
                    }
                    let remaining = resp.usage_limit() - resp.current_usage();
                    tm.update_balance_cache(id, remaining);
                    tracing::debug!("凭据 #{} 余额缓存已刷新: {:.2}", id, remaining);
                    if remaining < 1.0 {
                        tm.record_disable_event(
                            id,
                            crate::kiro::token_manager::DisableReason::InsufficientBalance,
                            None,
                            format!("余额不足 ({:.2} < 1.0)，已主动禁用", remaining),
                            None,
                            None,
                            None,
                            None,
                            None,
                        );
                        tm.mark_insufficient_balance(id);
                        tracing::warn!("凭据 #{} 余额不足 ({:.2})，已主动禁用", id, remaining);
                    }
                }
                Err(e) => {
                    tracing::warn!("凭据 #{} 余额刷新失败: {}", id, e);
                }
            }
        });
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移：
    /// - 400 Bad Request: 直接返回错误，不计入凭据失败
    /// - 401/403: 视为凭据/权限问题，计入失败次数并允许故障转移
    /// - 402 MONTHLY_REQUEST_COUNT: 视为额度用尽，禁用凭据并切换
    /// - 429/5xx/网络等瞬态错误: 重试但不禁用或切换凭据（避免误把所有凭据锁死）
    ///
    /// # Arguments
    /// * `request_body` - JSON 格式的请求体字符串
    ///
    /// # Returns
    /// 返回原始的 HTTP Response，不做解析
    pub async fn call_api(
        &self,
        request_body: &str,
        user_id: Option<&str>,
        allowed: Option<&std::collections::HashSet<u64>>,
    ) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, false, user_id, allowed)
            .await
    }

    /// 强制用指定凭据发一次（无重试 / 无故障转移；用于"对话测试"等场景）。
    /// 不计入冷却 / 失败计数；上游错误直接 bail，由调用方决定如何处理。
    pub async fn call_api_with_credential(
        &self,
        credential_id: u64,
        request_body: &str,
    ) -> anyhow::Result<ApiCallResult> {
        let ctx = self
            .token_manager
            .acquire_context_for_credential(credential_id)
            .await?;

        let _in_flight_guard = InFlightGuard::new(ctx.in_flight.clone());

        let config = self.token_manager.config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config)
            .ok_or_else(|| anyhow::anyhow!("无法生成 machine_id"))?;
        let endpoint = self.endpoint_for(&ctx.credentials)?;
        let request_ctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config: &config,
        };
        let url = endpoint.api_url(&request_ctx);
        let final_body = endpoint
            .transform_api_body(request_body, &request_ctx)
            .unwrap_or_else(|_| request_body.to_string());

        let client = self.get_client_for_credential(&ctx)?;
        let base_request = client
            .post(&url)
            .body(final_body)
            .header("content-type", "application/json");
        let request = endpoint.decorate_api(base_request, &request_ctx);
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("test-chat 上游 {} 错误: {}", status, body);
        }
        ctx.rpm.record();
        Ok(ApiCallResult {
            response,
            credential_id,
        })
    }

    /// 发送流式 API 请求
    ///
    /// 支持多凭据故障转移：
    /// - 400 Bad Request: 直接返回错误，不计入凭据失败
    /// - 401/403: 视为凭据/权限问题，计入失败次数并允许故障转移
    /// - 402 MONTHLY_REQUEST_COUNT: 视为额度用尽，禁用凭据并切换
    /// - 429/5xx/网络等瞬态错误: 重试但不禁用或切换凭据（避免误把所有凭据锁死）
    ///
    /// # Arguments
    /// * `request_body` - JSON 格式的请求体字符串
    ///
    /// # Returns
    /// 返回原始的 HTTP Response，调用方负责处理流式数据
    pub async fn call_api_stream(
        &self,
        request_body: &str,
        user_id: Option<&str>,
        allowed: Option<&std::collections::HashSet<u64>>,
    ) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, true, user_id, allowed)
            .await
    }

    /// 发送 MCP API 请求
    ///
    /// 用于 WebSearch 等工具调用
    ///
    /// # Arguments
    /// * `request_body` - JSON 格式的 MCP 请求体字符串
    ///
    /// # Returns
    /// 返回原始的 HTTP Response 以及实际使用的 credential_id
    pub async fn call_mcp(
        &self,
        request_body: &str,
        allowed: Option<&std::collections::HashSet<u64>>,
    ) -> anyhow::Result<McpCallResult> {
        self.call_mcp_with_retry(request_body, allowed).await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(
        &self,
        request_body: &str,
        allowed: Option<&std::collections::HashSet<u64>>,
    ) -> anyhow::Result<McpCallResult> {
        let total_credentials = self.token_manager.total_count();
        let available = self.token_manager.available_count();
        if available == 0 {
            anyhow::bail!("没有可用的凭据");
        }
        let max_retries =
            Self::compute_max_retries(&self.token_manager.config(), total_credentials);
        let mut last_error: Option<anyhow::Error> = None;
        let mut forced_token_refresh: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // 获取调用上下文（受 API Key 允许范围限制）
            let ctx = match self.token_manager.acquire_context_scoped(allowed).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            // 实时指标：进 in_flight（guard drop 时自动 dec）。RPM 仅在请求成功后再记录。
            let _in_flight_guard = InFlightGuard::new(ctx.in_flight.clone());

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config)
                .ok_or_else(|| anyhow::anyhow!("无法生成 machine_id，请检查凭证配置"))?;
            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let endpoint_name = endpoint.name();
            let request_ctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };
            let url = endpoint.mcp_url(&request_ctx);
            let body = match endpoint.transform_mcp_body(request_body, &request_ctx) {
                Ok(body) => body,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            tracing::debug!(
                credential_id = %ctx.id,
                endpoint = %endpoint_name,
                "发送 MCP 请求"
            );
            let client = match self.get_client_for_credential(&ctx) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        credential_id = %ctx.id,
                        "MCP 请求获取代理 client 失败（标记 ProxyExhausted 冷却）: {}",
                        e
                    );
                    self.token_manager.report_proxy_exhausted(ctx.id);
                    last_error = Some(UpstreamAttemptError::wrap(ctx.id, None, e));
                    continue;
                }
            };
            // 同 API 路径：不强制关连接，复用连接池减少握手开销
            let base_request = client
                .post(&url)
                .body(body)
                .header("content-type", "application/json");
            let request = endpoint.decorate_mcp(base_request, &request_ctx);
            #[cfg(feature = "sensitive-logs")]
            let _request_for_log = request.try_clone();

            // 发送请求
            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "MCP 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // 未收到上游响应：upstream_status=None，不计入上游错误统计
                    last_error = Some(UpstreamAttemptError::wrap(ctx.id, None, e.into()));
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = self.parse_retry_after(response.headers());
            // 统一错误包装：携带本次尝试的凭据 ID 与上游状态码。
            // 新增错误分支时一律用它包装（未收到上游响应的场景才手写 wrap(ctx.id, None, ..)）
            let wrap_err =
                |e: anyhow::Error| UpstreamAttemptError::wrap(ctx.id, Some(status.as_u16()), e);

            // 成功响应
            if status.is_success() {
                ctx.rpm.record();
                self.token_manager.report_success(ctx.id);
                tracing::info!(
                    credential_id = %ctx.id,
                    endpoint = %endpoint_name,
                    "MCP 请求成功"
                );
                return Ok(McpCallResult {
                    response,
                    credential_id: ctx.id,
                });
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();
            let display_body = self
                .token_manager
                .match_error_replacement(&body)
                .unwrap_or_else(|| body.clone());

            // 配置的"错误响应自动禁用"规则
            if let Some(pattern) = self.token_manager.match_auto_disable_pattern(&body) {
                tracing::warn!(
                    credential_id = ctx.id,
                    pattern = %pattern,
                    "MCP 错误响应命中自动禁用规则，凭据被永久禁用"
                );
                self.token_manager.record_disable_event(
                    ctx.id,
                    crate::kiro::token_manager::DisableReason::AccountSuspended,
                    Some(status.as_u16()),
                    format!("MCP 命中自动禁用规则 \"{}\"", pattern),
                    Some(request_body.to_string()),
                    Some(body.clone()),
                    Some("/mcp".to_string()),
                    None,
                    None,
                );
                self.token_manager.mark_account_suspended(ctx.id);
                self.token_manager.record_upstream_error(ctx.id);
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "MCP 命中自动禁用规则 \"{}\": {} {}",
                    pattern,
                    status,
                    display_body
                )));
                continue;
            }

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                self.token_manager.record_upstream_error(ctx.id);
                if !has_available {
                    return Err(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败（所有凭据已用尽）: {} {}",
                        status,
                        body
                    )));
                }
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                let is_too_long = Self::is_input_too_long(&body);
                // 输入过长错误：只记录请求体大小，不输出完整内容（太占空间且无调试价值）
                if is_too_long {
                    let body_bytes = request_body.len();
                    let estimated_tokens = Self::estimate_tokens(request_body);
                    tracing::error!(
                        status = %status,
                        response_body_bytes = body.len(),
                        request_url = %url,
                        request_body_bytes = body_bytes,
                        estimated_input_tokens = estimated_tokens,
                        "MCP 400 Bad Request - 输入上下文过长"
                    );
                } else {
                    // 其他 400 错误：记录请求信息以便调试
                    #[cfg(feature = "sensitive-logs")]
                    tracing::error!(
                        status = %status,
                        response_body = %body,
                        request_url = %url,
                        request_body_bytes = request_body.len(),
                        "MCP 400 Bad Request - 请求格式错误"
                    );
                    #[cfg(not(feature = "sensitive-logs"))]
                    tracing::error!(
                        status = %status,
                        response_body_bytes = body.len(),
                        request_url = %url,
                        request_body_bytes = request_body.len(),
                        "MCP 400 Bad Request - 请求格式错误"
                    );
                }
                self.token_manager.record_upstream_error(ctx.id);
                #[cfg(feature = "sensitive-logs")]
                return Err(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
                #[cfg(not(feature = "sensitive-logs"))]
                {
                    if is_too_long {
                        let body_bytes = request_body.len();
                        let estimated_tokens = Self::estimate_tokens(request_body);
                        return Err(wrap_err(anyhow::anyhow!(
                            "MCP 请求失败: {} Input is too long. (request_body_bytes={}, estimated_input_tokens={})",
                            status,
                            body_bytes,
                            estimated_tokens
                        )));
                    }

                    let summary = Self::summarize_error_body(&body);
                    return Err(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败: {} {}",
                        status,
                        summary
                    )));
                }
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                // bearer token 失效：优先触发刷新再重试（避免因 expiresAt 不准导致误判/误禁用）
                if endpoint.is_bearer_token_invalid(&body) && forced_token_refresh.insert(ctx.id) {
                    tracing::warn!(
                        "MCP 请求失败（Bearer token 无效，触发刷新后重试，尝试 {}/{}）: {} {}",
                        attempt + 1,
                        max_retries,
                        status,
                        body
                    );
                    // 计入凭据累计错误/分布条（这确实是一次上游 401/403），
                    // 但不走 report_failure——自动刷新重试的临时失效不应推进禁用计数
                    self.token_manager.record_upstream_error(ctx.id);
                    self.token_manager.invalidate_access_token(ctx.id);
                    last_error = Some(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败: {} {}",
                        status,
                        body
                    )));
                    continue;
                }

                // report_failure 已包含 error_count += 1，这里不再重复计数
                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    return Err(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败（所有凭据已用尽）: {} {}",
                        status,
                        body
                    )));
                }
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
                continue;
            }

            if status.as_u16() == 429 {
                // 429 计入 rate_limit_count（「超速」口径），不重复计入 error_count
                self.token_manager.report_rate_limit(ctx.id);
                if Self::is_model_temporarily_unavailable(&body)
                    && self.token_manager.report_model_unavailable()
                {
                    return Err(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败（模型暂时不可用，已触发熔断）: {} {}",
                        status,
                        body
                    )));
                }

                let cooldown = self.handle_rate_limited_response(ctx.id, &body, retry_after);
                tracing::warn!(
                    credential_id = %ctx.id,
                    cooldown_secs = %cooldown.as_secs(),
                    "MCP 请求触发 429，当前凭据进入冷却并尝试切换其他凭据"
                );
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
                continue;
            }

            // 瞬态错误
            if matches!(status.as_u16(), 408) || status.is_server_error() {
                tracing::warn!(
                    "MCP 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                self.token_manager.record_upstream_error(ctx.id);
                // 检测 MODEL_TEMPORARILY_UNAVAILABLE 并触发熔断机制
                if Self::is_model_temporarily_unavailable(&body)
                    && self.token_manager.report_model_unavailable()
                {
                    // 熔断已触发，所有凭据已禁用，立即返回错误
                    return Err(wrap_err(anyhow::anyhow!(
                        "MCP 请求失败（模型暂时不可用，已触发熔断）: {} {}",
                        status,
                        body
                    )));
                }

                last_error = Some(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                self.token_manager.record_upstream_error(ctx.id);
                return Err(wrap_err(anyhow::anyhow!(
                    "MCP 请求失败: {} {}",
                    status,
                    body
                )));
            }

            // 兜底
            self.token_manager.record_upstream_error(ctx.id);
            last_error = Some(wrap_err(anyhow::anyhow!(
                "MCP 请求失败: {} {}",
                status,
                body
            )));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 总重试次数 = min(凭据数量 × 每凭据重试次数, MAX_TOTAL_RETRIES)
    /// - 硬上限 3 次，避免无限重试
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
        user_id: Option<&str>,
        allowed: Option<&std::collections::HashSet<u64>>,
    ) -> anyhow::Result<ApiCallResult> {
        let total_credentials = self.token_manager.total_count();
        let available = self.token_manager.available_count();
        if available == 0 {
            anyhow::bail!("没有可用的凭据");
        }
        let max_retries =
            Self::compute_max_retries(&self.token_manager.config(), total_credentials);
        let mut last_error: Option<anyhow::Error> = None;
        let mut forced_token_refresh: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token），支持用户亲和性 + API Key 允许范围
            let ctx = match self
                .token_manager
                .acquire_context_for_user(user_id, allowed)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            // 实时指标：进 in_flight（guard drop 时自动 dec）。RPM 仅在请求成功后再记录。
            let _in_flight_guard = InFlightGuard::new(ctx.in_flight.clone());

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config)
                .ok_or_else(|| anyhow::anyhow!("无法生成 machine_id，请检查凭证配置"))?;
            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let endpoint_name = endpoint.name();
            let request_ctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };
            let url = endpoint.api_url(&request_ctx);
            let final_body = match endpoint.transform_api_body(request_body, &request_ctx) {
                Ok(body) => body,
                Err(e) => {
                    tracing::warn!("变换 endpoint 请求体失败，使用原始请求体: {}", e);
                    request_body.to_string()
                }
            };
            let final_body_for_log = final_body.clone();

            tracing::debug!(
                credential_id = %ctx.id,
                endpoint = %endpoint_name,
                "发送 {} API 请求",
                api_type
            );

            // 获取凭据对应的 client（启用代理池时强制走代理槽，不允许回退本地）
            let client = match self.get_client_for_credential(&ctx) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        credential_id = %ctx.id,
                        "API 请求获取代理 client 失败（标记 ProxyExhausted 冷却）: {}",
                        e
                    );
                    self.token_manager.report_proxy_exhausted(ctx.id);
                    last_error = Some(UpstreamAttemptError::wrap(ctx.id, None, e));
                    continue;
                }
            };
            // 不再强制 Connection: close——保留连接以命中 reqwest 连接池
            //（pool_idle_timeout=90s），避免每次请求都重做 TCP+TLS 握手拖慢首字。
            let base_request = client
                .post(&url)
                .body(final_body)
                .header("content-type", "application/json");
            let request = endpoint.decorate_api(base_request, &request_ctx);
            #[cfg(feature = "sensitive-logs")]
            let _request_for_log = request.try_clone();

            // 发送请求
            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "API 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // 网络错误通常是上游/链路瞬态问题，不应导致"禁用凭据"或"切换凭据"
                    // （否则一段时间网络抖动会把所有凭据都误禁用，需要重启才能恢复）
                    // 未收到上游响应：upstream_status=None，不计入上游错误统计
                    last_error = Some(UpstreamAttemptError::wrap(ctx.id, None, e.into()));
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = self.parse_retry_after(response.headers());
            // 统一错误包装：携带本次尝试的凭据 ID 与上游状态码。
            // 新增错误分支时一律用它包装（未收到上游响应的场景才手写 wrap(ctx.id, None, ..)）
            let wrap_err =
                |e: anyhow::Error| UpstreamAttemptError::wrap(ctx.id, Some(status.as_u16()), e);

            // 成功响应
            if status.is_success() {
                ctx.rpm.record();
                self.token_manager.report_success(ctx.id);
                tracing::info!(
                    credential_id = %ctx.id,
                    endpoint = %endpoint_name,
                    "API 请求成功"
                );
                // 后台异步刷新余额缓存
                self.spawn_balance_refresh(ctx.id);
                return Ok(ApiCallResult {
                    response,
                    credential_id: ctx.id,
                });
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            // 错误内容替换规则：用于改变返回给客户端的错误体（不影响内部判断）
            let display_body = self
                .token_manager
                .match_error_replacement(&body)
                .unwrap_or_else(|| body.clone());

            // 配置的"错误响应自动禁用"规则：命中任一关键字则永久禁用该凭据
            if let Some(pattern) = self.token_manager.match_auto_disable_pattern(&body) {
                tracing::warn!(
                    credential_id = ctx.id,
                    pattern = %pattern,
                    "错误响应命中自动禁用规则，凭据被永久禁用"
                );
                self.token_manager.record_disable_event(
                    ctx.id,
                    crate::kiro::token_manager::DisableReason::AccountSuspended,
                    Some(status.as_u16()),
                    format!("{} 命中自动禁用规则 \"{}\"", api_type, pattern),
                    Some(final_body_for_log.clone()),
                    Some(body.clone()),
                    Some("/v1/messages".to_string()),
                    None,
                    user_id.map(|s| s.to_string()),
                );
                self.token_manager.mark_account_suspended(ctx.id);
                self.token_manager.record_upstream_error(ctx.id);
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "{} 命中自动禁用规则 \"{}\": {} {}",
                    api_type,
                    pattern,
                    status,
                    display_body
                )));
                continue;
            }

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    "API 请求失败（额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                self.token_manager.record_upstream_error(ctx.id);
                self.token_manager.update_balance_cache(ctx.id, 0.0);
                if !has_available {
                    return Err(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    )));
                }

                last_error = Some(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
                continue;
            }

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                let is_too_long = Self::is_input_too_long(&body);
                // 输入过长错误：只记录请求体大小，不输出完整内容（太占空间且无调试价值）
                if is_too_long {
                    let body_bytes = final_body_for_log.len();
                    let estimated_tokens = Self::estimate_tokens(&final_body_for_log);
                    tracing::error!(
                        status = %status,
                        response_body_bytes = body.len(),
                        request_url = %url,
                        request_body_bytes = body_bytes,
                        estimated_input_tokens = estimated_tokens,
                        "400 Bad Request - 输入上下文过长"
                    );
                } else {
                    // 其他 400 错误：记录请求信息以便调试
                    #[cfg(feature = "sensitive-logs")]
                    tracing::error!(
                        status = %status,
                        response_body = %body,
                        request_url = %url,
                        request_body = %Self::truncate_body_for_log(&final_body_for_log, 1200),
                        "400 Bad Request - 请求格式错误"
                    );
                    #[cfg(not(feature = "sensitive-logs"))]
                    tracing::error!(
                        status = %status,
                        response_body_bytes = body.len(),
                        request_url = %url,
                        request_body_bytes = final_body_for_log.len(),
                        "400 Bad Request - 请求格式错误"
                    );
                }
                self.token_manager.record_upstream_error(ctx.id);
                #[cfg(feature = "sensitive-logs")]
                return Err(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
                #[cfg(not(feature = "sensitive-logs"))]
                {
                    // 对用户保留可区分的错误信息（例如 Input is too long），但避免返回过长内容。
                    if is_too_long {
                        let body_bytes = final_body_for_log.len();
                        let estimated_tokens = Self::estimate_tokens(&final_body_for_log);
                        return Err(wrap_err(anyhow::anyhow!(
                            "{} API 请求失败: {} Input is too long. (request_body_bytes={}, estimated_input_tokens={})",
                            api_type,
                            status,
                            body_bytes,
                            estimated_tokens
                        )));
                    }

                    let summary = Self::summarize_error_body(&body);
                    return Err(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败: {} {}",
                        api_type,
                        status,
                        summary
                    )));
                }
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                // bearer token 失效：优先触发刷新再重试（避免因 expiresAt 不准导致误判/误禁用）
                if endpoint.is_bearer_token_invalid(&body) && forced_token_refresh.insert(ctx.id) {
                    tracing::warn!(
                        "API 请求失败（Bearer token 无效，触发刷新后重试，尝试 {}/{}）: {} {}",
                        attempt + 1,
                        max_retries,
                        status,
                        body
                    );
                    // 计入凭据累计错误/分布条（这确实是一次上游 401/403），
                    // 但不走 report_failure——自动刷新重试的临时失效不应推进禁用计数
                    self.token_manager.record_upstream_error(ctx.id);
                    self.token_manager.invalidate_access_token(ctx.id);
                    last_error = Some(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败: {} {}",
                        api_type,
                        status,
                        body
                    )));
                    continue;
                }

                tracing::warn!(
                    "API 请求失败（可能为凭据错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                // report_failure 已包含 error_count += 1，这里不再重复计数
                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    return Err(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    )));
                }

                last_error = Some(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
                continue;
            }

            if status.as_u16() == 429 {
                // 429 计入 rate_limit_count（「超速」口径），不重复计入 error_count
                self.token_manager.report_rate_limit(ctx.id);
                if Self::is_model_temporarily_unavailable(&body)
                    && self.token_manager.report_model_unavailable()
                {
                    return Err(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败（模型暂时不可用，已触发熔断）: {} {}",
                        api_type,
                        status,
                        body
                    )));
                }

                let cooldown = self.handle_rate_limited_response(ctx.id, &body, retry_after);
                tracing::warn!(
                    credential_id = %ctx.id,
                    cooldown_secs = %cooldown.as_secs(),
                    "{} API 请求触发 429，当前凭据进入冷却并尝试切换其他凭据",
                    api_type
                );
                last_error = Some(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
                continue;
            }

            // 408/5xx - 瞬态上游错误：重试但不禁用或切换凭据
            // （避免 502 high load 等瞬态错误把所有凭据锁死）
            if matches!(status.as_u16(), 408) || status.is_server_error() {
                tracing::warn!(
                    "API 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                self.token_manager.record_upstream_error(ctx.id);
                if Self::is_model_temporarily_unavailable(&body)
                    && self.token_manager.report_model_unavailable()
                {
                    return Err(wrap_err(anyhow::anyhow!(
                        "{} API 请求失败（模型暂时不可用，已触发熔断）: {} {}",
                        api_type,
                        status,
                        body
                    )));
                }

                last_error = Some(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据连续失败
            if status.is_client_error() {
                self.token_manager.record_upstream_error(ctx.id);
                return Err(wrap_err(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                )));
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            tracing::warn!(
                "API 请求失败（未知错误，尝试 {}/{}）: {} {}",
                attempt + 1,
                max_retries,
                status,
                body
            );
            self.token_manager.record_upstream_error(ctx.id);
            last_error = Some(wrap_err(anyhow::anyhow!(
                "{} API 请求失败: {} {}",
                api_type,
                status,
                body
            )));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
    }

    /// 根据配置和当前凭据数计算本次请求的最大重试次数（每凭据次数 × 凭据数，
    /// 受 `max_total_retries` 上限约束；保证至少为 1，避免误配置导致一次都不试）。
    fn compute_max_retries(
        config: &crate::model::config::Config,
        total_credentials: usize,
    ) -> usize {
        let per_cred = if config.max_retries_per_credential == 0 {
            DEFAULT_MAX_RETRIES_PER_CREDENTIAL
        } else {
            config.max_retries_per_credential as usize
        };
        let total_cap = if config.max_total_retries == 0 {
            DEFAULT_MAX_TOTAL_RETRIES
        } else {
            config.max_total_retries as usize
        };
        let raw = total_credentials.saturating_mul(per_cred).min(total_cap);
        raw.max(1)
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }

    fn handle_rate_limited_response(
        &self,
        credential_id: u64,
        body: &str,
        retry_after: Option<Duration>,
    ) -> Duration {
        // 容量类 429（INSUFFICIENT_MODEL_CAPACITY / "high traffic"）是上游侧瞬时
        // 扩容问题，不是某个凭据真的被限流。给个短冷却即可（下次仍可被选中），
        // 避免把所有凭据按 60s 标准冷却轮一遍后误报"所有凭据均处于冷却"。
        let is_capacity_pressure = Self::is_insufficient_model_capacity(body);

        let cfg = self.token_manager.config();

        // 全局开关：关闭所有 429 冷却。
        // 仅触发"换号重试"，凭据不进入冷却状态，下一次轮询立即可被再次选中。
        if cfg.rate_limit_disable_cooldown {
            tracing::warn!(
                credential_id = %credential_id,
                rate_limit_response = %Self::is_rate_limit_response(body),
                capacity_pressure = %is_capacity_pressure,
                "凭据触发 429，rate_limit_disable_cooldown=true，跳过冷却"
            );
            return Duration::ZERO;
        }
        let capacity_secs = if cfg.capacity_pressure_cooldown_secs == 0 {
            CAPACITY_RATE_LIMIT_COOLDOWN_SECS
        } else {
            cfg.capacity_pressure_cooldown_secs
        };
        let configured_min_secs = if cfg.rate_limit_cooldown_min_secs == 0 {
            DEFAULT_RATE_LIMIT_COOLDOWN_SECS
        } else {
            cfg.rate_limit_cooldown_min_secs
        };
        let configured_max_secs = {
            let m = if cfg.rate_limit_cooldown_max_secs == 0 {
                MAX_RATE_LIMIT_COOLDOWN_SECS
            } else {
                cfg.rate_limit_cooldown_max_secs
            };
            m.max(configured_min_secs) // 防御：max < min 时退化
        };

        // 关键：custom_duration 必须是 Some，否则 CooldownManager 会用
        // RateLimitExceeded.default_duration() = 60s 这个写死值 + 1.5 倍递增，
        // 完全无视配置。
        let custom_duration = if is_capacity_pressure {
            // 容量类：优先 Retry-After；否则用容量短冷却（不被 [min,max] 拉伸）
            Some(retry_after.unwrap_or_else(|| Duration::from_secs(capacity_secs)))
        } else if cfg.rate_limit_ignore_retry_after {
            // 用户开启「忽略 Retry-After」：完全无视上游头，直接在 [min,max] 内随机
            let secs = if configured_min_secs == configured_max_secs {
                configured_min_secs
            } else {
                fastrand::u64(configured_min_secs..=configured_max_secs)
            };
            Some(Duration::from_secs(secs))
        } else {
            // 普通 429：上游带 Retry-After 已被 parse_retry_after clamp 到 [min,max]；
            // 不带时用 configured_min_secs 作为基线，再 clamp（防御 min>max 等异常）
            let base = retry_after.unwrap_or_else(|| Duration::from_secs(configured_min_secs));
            Some(self.clamp_rate_limit_cooldown(base))
        };

        let cooldown = self.token_manager.set_credential_cooldown_with_duration(
            credential_id,
            crate::kiro::cooldown::CooldownReason::RateLimitExceeded,
            custom_duration,
        );

        tracing::warn!(
            credential_id = %credential_id,
            retry_after_secs = ?retry_after.map(|d| d.as_secs()),
            cooldown_secs = %cooldown.as_secs(),
            rate_limit_response = %Self::is_rate_limit_response(body),
            capacity_pressure = %is_capacity_pressure,
            "凭据触发 429 限流，已设置冷却"
        );

        cooldown
    }

    /// 检测是否为容量瓶颈型 429（INSUFFICIENT_MODEL_CAPACITY / "high traffic"）。
    /// 这类错误代表上游模型暂时容量不足，所有凭据看到的状态相同；
    /// 不应当作单凭据"被限流"标准 60s 冷却处理。
    fn is_insufficient_model_capacity(body: &str) -> bool {
        let lower = body.to_ascii_lowercase();
        if lower.contains("insufficient_model_capacity")
            || lower.contains("high traffic")
            || lower.contains("experiencing high traffic")
        {
            return true;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };
        let reason_matches = |s: &str| {
            let upper = s.to_ascii_uppercase();
            upper.contains("INSUFFICIENT_MODEL_CAPACITY") || upper.contains("HIGH_TRAFFIC")
        };
        value
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(reason_matches)
            || value
                .pointer("/error/reason")
                .and_then(|v| v.as_str())
                .is_some_and(reason_matches)
    }

    fn parse_retry_after(&self, headers: &HeaderMap) -> Option<Duration> {
        let raw = headers.get("retry-after")?.to_str().ok()?.trim();
        if raw.is_empty() {
            return None;
        }

        if let Ok(seconds) = raw.parse::<u64>() {
            return Some(self.clamp_rate_limit_cooldown(Duration::from_secs(seconds)));
        }

        let retry_at = DateTime::parse_from_rfc2822(raw).ok()?.with_timezone(&Utc);
        let now = Utc::now();
        let wait = retry_at.signed_duration_since(now).to_std().ok()?;
        Some(self.clamp_rate_limit_cooldown(wait))
    }

    fn clamp_rate_limit_cooldown(&self, duration: Duration) -> Duration {
        let cfg = self.token_manager.config();
        let min = if cfg.rate_limit_cooldown_min_secs == 0 {
            DEFAULT_RATE_LIMIT_COOLDOWN_SECS
        } else {
            cfg.rate_limit_cooldown_min_secs
        };
        let max = if cfg.rate_limit_cooldown_max_secs == 0 {
            MAX_RATE_LIMIT_COOLDOWN_SECS
        } else {
            cfg.rate_limit_cooldown_max_secs
        };
        // 防止用户配反：min > max 时退化为 [min, min]
        let max = max.max(min);
        duration.clamp(Duration::from_secs(min), Duration::from_secs(max))
    }

    fn is_rate_limit_response(body: &str) -> bool {
        let lower = body.to_ascii_lowercase();
        if lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("high traffic")
            || lower.contains("request limit")
        {
            return true;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };

        let reason_matches = |s: &str| {
            let upper = s.to_ascii_uppercase();
            upper.contains("RATE_LIMIT")
                || upper.contains("TOO_MANY_REQUESTS")
                || upper.contains("REQUEST_LIMIT")
                || upper.contains("HIGH_TRAFFIC")
        };

        value
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(reason_matches)
            || value
                .pointer("/error/reason")
                .and_then(|v| v.as_str())
                .is_some_and(reason_matches)
    }

    /// 检测是否为 MODEL_TEMPORARILY_UNAVAILABLE 错误
    fn is_model_temporarily_unavailable(body: &str) -> bool {
        if body.contains("MODEL_TEMPORARILY_UNAVAILABLE") {
            return true;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };

        if value
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == "MODEL_TEMPORARILY_UNAVAILABLE")
        {
            return true;
        }

        value
            .pointer("/error/reason")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == "MODEL_TEMPORARILY_UNAVAILABLE")
    }

    /// 检测是否为「输入过长」类错误
    ///
    /// 典型返回：
    /// `{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}`
    fn is_input_too_long(body: &str) -> bool {
        body.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") || body.contains("Input is too long")
    }

    /// 从上游响应体提取一个适合返回给客户端的错误摘要
    ///
    /// 目标：
    /// - 保留关键错误信息（例如 "Input is too long" / "Improperly formed request"）
    /// - 避免返回过长/不可控的内容导致客户端难以区分或处理
    #[cfg(not(feature = "sensitive-logs"))]
    fn summarize_error_body(body: &str) -> String {
        const MAX_LEN: usize = 256;
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return "<empty response body>".to_string();
        }

        // 优先尝试解析 JSON，从常见字段中提取 message / reason。
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let message = value
                .get("message")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("Message").and_then(|v| v.as_str()))
                .or_else(|| value.pointer("/error/message").and_then(|v| v.as_str()))
                .or_else(|| value.pointer("/error/Message").and_then(|v| v.as_str()));

            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("Reason").and_then(|v| v.as_str()))
                .or_else(|| value.pointer("/error/reason").and_then(|v| v.as_str()))
                .or_else(|| value.pointer("/error/Reason").and_then(|v| v.as_str()));

            if let Some(msg) = message {
                let mut s = msg.to_string();
                if let Some(r) = reason.filter(|r| !r.is_empty() && *r != "null") {
                    // 避免重复拼接（有些上游会把 reason 直接写入 message）
                    if !msg.contains(r) {
                        s.push_str(&format!(" (reason={})", r));
                    }
                }
                return Self::truncate_one_line(&s, MAX_LEN);
            }
        }

        // JSON 解析失败或不含常见字段，回退到压缩后的纯文本。
        Self::truncate_one_line(trimmed, MAX_LEN)
    }

    #[cfg(not(feature = "sensitive-logs"))]
    fn truncate_one_line(s: &str, max_len: usize) -> String {
        let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.len() <= max_len {
            return one_line;
        }

        let end = floor_char_boundary(&one_line, max_len);
        format!("{}...", &one_line[..end])
    }

    /// 估算文本的 token 数量
    ///
    /// 基于字符类型的估算公式：
    /// - CJK 字符（中/日/韩）: token 数 = 字符数 / 1.5
    /// - 其他字符（英文等）: token 数 = 字符数 / 3.5
    fn estimate_tokens(text: &str) -> usize {
        let mut cjk_count = 0usize;
        let mut other_count = 0usize;

        for c in text.chars() {
            if Self::is_cjk_char(c) {
                cjk_count += 1;
            } else {
                other_count += 1;
            }
        }

        let cjk_tokens = cjk_count as f64 / 1.5;
        let other_tokens = other_count as f64 / 3.5;
        (cjk_tokens + other_tokens + 0.5) as usize
    }

    /// 判断是否为 CJK（中日韩）字符
    #[inline]
    fn is_cjk_char(c: char) -> bool {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'   |  // CJK 统一汉字
            '\u{3400}'..='\u{4DBF}'   |  // CJK 扩展 A
            '\u{20000}'..='\u{2A6DF}' |  // CJK 扩展 B
            '\u{2A700}'..='\u{2B73F}' |  // CJK 扩展 C
            '\u{2B740}'..='\u{2B81F}' |  // CJK 扩展 D
            '\u{F900}'..='\u{FAFF}'   |  // CJK 兼容汉字
            '\u{2F800}'..='\u{2FA1F}' |  // CJK 兼容扩展
            '\u{3000}'..='\u{303F}'   |  // CJK 标点符号
            '\u{3040}'..='\u{309F}'   |  // 平假名
            '\u{30A0}'..='\u{30FF}'   |  // 片假名
            '\u{AC00}'..='\u{D7AF}'      // 韩文音节
        )
    }

    /// 截断请求体用于日志输出，保留头尾各 `keep` 个字符
    ///
    /// 避免在 sensitive-logs 模式下输出包含大量 base64 图片数据的完整请求体。
    #[cfg(feature = "sensitive-logs")]
    fn truncate_body_for_log(s: &str, keep: usize) -> std::borrow::Cow<'_, str> {
        let char_count = s.chars().count();
        let min_omit = 30;
        if char_count <= keep * 2 + min_omit {
            return std::borrow::Cow::Borrowed(s);
        }

        let head_end = s
            .char_indices()
            .nth(keep)
            .map(|(i, _)| i)
            .unwrap_or(s.len());

        let tail_start = s
            .char_indices()
            .nth_back(keep - 1)
            .map(|(i, _)| i)
            .unwrap_or(0);

        let omitted = s.len() - head_end - (s.len() - tail_start);
        std::borrow::Cow::Owned(format!(
            "{}...({} bytes omitted)...{}",
            &s[..head_end],
            omitted,
            &s[tail_start..]
        ))
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::kiro::cooldown::CooldownReason;
    use crate::kiro::endpoint::{
        CliEndpoint, IdeEndpoint, default_is_bearer_token_invalid, default_is_monthly_request_limit,
    };
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;
    use reqwest::header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HeaderValue};

    fn create_test_provider(config: Config, credentials: KiroCredentials) -> KiroProvider {
        let tm = MultiTokenManager::new(config, vec![credentials], None, None, false).unwrap();
        KiroProvider::new(Arc::new(tm))
    }

    #[test]
    fn test_cli_endpoint_api_url() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let endpoint = CliEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        assert!(endpoint.api_url(&ctx).contains("amazonaws.com"));
        assert!(endpoint.api_url(&ctx).contains("generateAssistantResponse"));
    }

    #[test]
    fn test_cli_endpoint_decorate_api_headers() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();

        let credentials = KiroCredentials::default();
        let endpoint = CliEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request = endpoint.decorate_api(
            reqwest::Client::new()
                .post("https://example.com")
                .header("content-type", "application/json"),
            &ctx,
        );
        let built = request.build().unwrap();

        assert_eq!(
            built.headers().get("x-amz-target").unwrap(),
            "AmazonQDeveloperStreamingService.SendMessage"
        );
        assert_eq!(
            built.headers().get("x-amzn-codewhisperer-optout").unwrap(),
            "false"
        );
        assert_eq!(
            built.headers().get("x-amzn-kiro-agent-mode").unwrap(),
            "cli"
        );
        // 不再强制 Connection: close（改为复用连接池）；decorate 不应注入该头
        assert!(built.headers().get(CONNECTION).is_none());
    }

    #[test]
    fn test_cli_endpoint_transform_api_body_rewrites_origin() {
        let endpoint = CliEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request_body = r#"{"conversationState":{"currentMessage":{"userInputMessage":{"origin":"AI_EDITOR"}},"history":[{"userInputMessage":{"origin":"AI_EDITOR"}}]}}"#;
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
            "KIRO_CLI"
        );
        assert_eq!(
            parsed["conversationState"]["history"][0]["userInputMessage"]["origin"],
            "KIRO_CLI"
        );
    }

    #[test]
    fn test_ide_endpoint_api_url() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        assert!(endpoint.api_url(&ctx).contains("amazonaws.com"));
        assert!(endpoint.api_url(&ctx).contains("generateAssistantResponse"));
    }

    #[test]
    fn test_ide_endpoint_host_like_domain() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();
        let credentials = KiroCredentials::default();
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request =
            endpoint.decorate_api(reqwest::Client::new().post("https://example.com"), &ctx);
        let built = request.build().unwrap();
        assert_eq!(
            built.headers().get("host").unwrap(),
            "q.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn test_ide_endpoint_decorate_api_headers() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();
        config.kiro_version = "0.8.0".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.profile_arn = Some("arn:aws:sso::123456789:profile/test".to_string());
        credentials.refresh_token = Some("a".repeat(150));

        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request = endpoint.decorate_api(
            reqwest::Client::new()
                .post("https://example.com")
                .header("content-type", "application/json"),
            &ctx,
        );
        let built = request.build().unwrap();

        assert_eq!(
            built.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            built.headers().get("x-amzn-codewhisperer-optout").unwrap(),
            "true"
        );
        assert_eq!(
            built.headers().get("x-amzn-kiro-agent-mode").unwrap(),
            "vibe"
        );
        assert!(
            built
                .headers()
                .get(AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("Bearer ")
        );
        // 不再强制 Connection: close（改为复用连接池）；decorate 不应注入该头
        assert!(built.headers().get(CONNECTION).is_none());
    }

    #[test]
    fn test_ide_endpoint_decorate_api_sets_tokentype() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();
        config.kiro_version = "0.8.0".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("api_key".to_string());
        credentials.kiro_api_key = Some("ksk_test_api_key".to_string());
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "ksk_test_api_key",
            machine_id: &machine_id,
            config: &config,
        };
        let request =
            endpoint.decorate_api(reqwest::Client::new().post("https://example.com"), &ctx);
        let built = request.build().unwrap();
        assert_eq!(built.headers().get("tokentype").unwrap(), "API_KEY");
    }

    #[test]
    fn test_ide_endpoint_decorate_mcp_includes_profile_arn_for_social_auth() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();
        config.kiro_version = "0.8.0".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("social".to_string());
        credentials.profile_arn = Some("arn:aws:sso::123456789:profile/test".to_string());
        credentials.refresh_token = Some("a".repeat(150));
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request =
            endpoint.decorate_mcp(reqwest::Client::new().post("https://example.com"), &ctx);
        let built = request.build().unwrap();
        assert_eq!(
            built
                .headers()
                .get("x-amzn-kiro-profile-arn")
                .unwrap()
                .to_str()
                .unwrap(),
            "arn:aws:sso::123456789:profile/test"
        );
    }

    #[test]
    fn test_ide_endpoint_decorate_mcp_omits_profile_arn_for_idc_auth() {
        let mut config = Config::default();
        config.region = "us-east-1".to_string();
        config.kiro_version = "0.8.0".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("idc".to_string());
        credentials.profile_arn = Some("arn:aws:sso::123456789:profile/test".to_string());
        credentials.client_id = Some("client".to_string());
        credentials.client_secret = Some("secret".to_string());
        credentials.refresh_token = Some("a".repeat(150));
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let request =
            endpoint.decorate_mcp(reqwest::Client::new().post("https://example.com"), &ctx);
        let built = request.build().unwrap();
        assert!(built.headers().get("x-amzn-kiro-profile-arn").is_none());
    }

    #[test]
    fn test_parse_retry_after_seconds() {
        let provider = create_test_provider(Config::default(), KiroCredentials::default());
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("120"));

        let wait = provider.parse_retry_after(&headers).unwrap();
        assert_eq!(wait, Duration::from_secs(120));
    }

    #[test]
    fn test_parse_retry_after_http_date() {
        let provider = create_test_provider(Config::default(), KiroCredentials::default());
        let mut headers = HeaderMap::new();
        let future = (Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
        headers.insert("retry-after", HeaderValue::from_str(&future).unwrap());

        let wait = provider.parse_retry_after(&headers).unwrap();
        assert!(wait >= Duration::from_secs(60));
        assert!(wait <= Duration::from_secs(120));
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        let provider = create_test_provider(Config::default(), KiroCredentials::default());
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("not-a-date"));

        assert!(provider.parse_retry_after(&headers).is_none());
    }

    #[test]
    fn test_parse_retry_after_clamps_range() {
        let provider = create_test_provider(Config::default(), KiroCredentials::default());
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("5"));
        assert_eq!(
            provider.parse_retry_after(&headers).unwrap(),
            Duration::from_secs(60)
        );

        headers.insert("retry-after", HeaderValue::from_static("600"));
        assert_eq!(
            provider.parse_retry_after(&headers).unwrap(),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn test_parse_retry_after_respects_config_min() {
        let mut config = Config::default();
        config.rate_limit_cooldown_min_secs = 10;
        config.rate_limit_cooldown_max_secs = 600;
        let provider = create_test_provider(config, KiroCredentials::default());
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("5"));
        assert_eq!(
            provider.parse_retry_after(&headers).unwrap(),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn test_upstream_attempt_error_display_and_downcast() {
        // Display 必须完全透传内部错误文本（handler 层按字符串分类）
        let inner = anyhow::anyhow!("流式 API 请求失败: 429 Too Many Requests high traffic");
        let wrapped = UpstreamAttemptError::wrap(7, Some(429), inner);
        assert_eq!(
            wrapped.to_string(),
            "流式 API 请求失败: 429 Too Many Requests high traffic"
        );

        // downcast 取出凭据 ID 与上游状态码（用于错误日志和计数口径）
        let attempt = wrapped.downcast_ref::<UpstreamAttemptError>().unwrap();
        assert_eq!(attempt.credential_id, Some(7));
        assert_eq!(attempt.upstream_status, Some(429));
        assert!(UpstreamAttemptError::reached_upstream(&wrapped));

        // 网络错误：无上游状态码 → 不算走到上游
        let net = UpstreamAttemptError::wrap(3, None, anyhow::anyhow!("error sending request"));
        let attempt = net.downcast_ref::<UpstreamAttemptError>().unwrap();
        assert_eq!(attempt.upstream_status, None);
        assert!(!UpstreamAttemptError::reached_upstream(&net));

        // 未包装的普通错误（本地错误）→ 不算走到上游
        assert!(!UpstreamAttemptError::reached_upstream(&anyhow::anyhow!(
            "没有可用的凭据"
        )));
    }

    #[test]
    fn test_is_rate_limit_response_detects_reason() {
        let body = r#"{"message":"Too many requests","reason":"RATE_LIMIT_EXCEEDED"}"#;
        assert!(KiroProvider::is_rate_limit_response(body));
    }

    #[test]
    fn test_is_rate_limit_response_detects_nested_reason() {
        let body = r#"{"error":{"reason":"REQUEST_LIMIT_5_MINUTES"}}"#;
        assert!(KiroProvider::is_rate_limit_response(body));
    }

    #[test]
    fn test_is_rate_limit_response_false() {
        let body = r#"{"message":"Forbidden","reason":"AUTH_FAILED"}"#;
        assert!(!KiroProvider::is_rate_limit_response(body));
    }

    #[test]
    fn test_handle_rate_limited_response_sets_cooldown() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let provider = create_test_provider(config, credentials);
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("120"));

        let cooldown = provider.handle_rate_limited_response(
            1,
            "Too many requests",
            provider.parse_retry_after(&headers),
        );
        assert_eq!(cooldown, Duration::from_secs(120));

        let (reason, remaining) = provider
            .token_manager()
            .cooldown_manager()
            .check_cooldown(1)
            .unwrap();
        assert_eq!(reason, CooldownReason::RateLimitExceeded);
        assert!(remaining <= Duration::from_secs(120));
        assert!(remaining > Duration::from_secs(100));

        let snapshot = provider.token_manager().snapshot();
        assert_eq!(snapshot.entries[0].failure_count, 0);
        assert!(!snapshot.entries[0].disabled);
        assert!(snapshot.entries[0].last_used_at.is_some());
    }

    #[test]
    fn test_handle_rate_limited_response_without_retry_after_uses_default_cooldown() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let provider = create_test_provider(config, credentials);

        let cooldown = provider.handle_rate_limited_response(1, "Too many requests", None);
        assert_eq!(cooldown, Duration::from_secs(60));

        let (reason, remaining) = provider
            .token_manager()
            .cooldown_manager()
            .check_cooldown(1)
            .unwrap();
        assert_eq!(reason, CooldownReason::RateLimitExceeded);
        assert!(remaining <= Duration::from_secs(60));
        assert!(remaining > Duration::from_secs(50));
    }

    /// 开启 `rate_limit_ignore_retry_after` 后，即使上游带了 Retry-After
    /// 也不应该使用，而是在 [min,max] 内随机
    #[test]
    fn test_handle_rate_limited_response_ignores_retry_after_when_toggled() {
        let mut config = Config::default();
        config.rate_limit_cooldown_min_secs = 15;
        config.rate_limit_cooldown_max_secs = 30;
        config.rate_limit_ignore_retry_after = true;
        let credentials = KiroCredentials::default();
        let provider = create_test_provider(config, credentials);

        // 上游说要等 7200 秒，应被忽略；冷却必须落在 [15, 30]
        for _ in 0..20 {
            let cooldown = provider.handle_rate_limited_response(
                1,
                "Too many requests",
                Some(Duration::from_secs(7200)),
            );
            assert!(
                cooldown >= Duration::from_secs(15) && cooldown <= Duration::from_secs(30),
                "cooldown {} 应在 [15, 30] 内",
                cooldown.as_secs()
            );
        }
    }

    /// 回归：没有 Retry-After 时，普通 429 必须用配置的 min（之前会走
    /// CooldownManager 默认的 60s + 1.5 倍递增，完全无视配置）
    #[test]
    fn test_handle_rate_limited_response_uses_configured_min_when_no_retry_after() {
        let mut config = Config::default();
        config.rate_limit_cooldown_min_secs = 15;
        config.rate_limit_cooldown_max_secs = 30;
        let credentials = KiroCredentials::default();
        let provider = create_test_provider(config, credentials);

        // 连续触发 5 次都应该是 15s，不会按 1.5 倍递增超过配置
        for _ in 0..5 {
            let cooldown = provider.handle_rate_limited_response(1, "Too many requests", None);
            assert_eq!(cooldown, Duration::from_secs(15));
        }
    }

    #[test]
    fn test_is_monthly_request_limit_detects_reason() {
        let body = r#"{"message":"You have reached the limit.","reason":"MONTHLY_REQUEST_COUNT"}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_is_monthly_request_limit_nested_reason() {
        let body = r#"{"error":{"reason":"MONTHLY_REQUEST_COUNT"}}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_is_monthly_request_limit_false() {
        let body = r#"{"message":"nope","reason":"DAILY_REQUEST_COUNT"}"#;
        assert!(!default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_is_invalid_bearer_token_true() {
        let body =
            r#"{"message":"The bearer token included in the request is invalid.","reason":null}"#;
        assert!(default_is_bearer_token_invalid(body));
    }

    #[test]
    fn test_is_invalid_bearer_token_false() {
        let body = r#"{"message":"Forbidden","reason":null}"#;
        assert!(!default_is_bearer_token_invalid(body));
    }

    #[test]
    #[cfg(not(feature = "sensitive-logs"))]
    fn test_summarize_error_body_extracts_message_and_reason() {
        let body =
            r#"{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#;
        let summary = KiroProvider::summarize_error_body(body);
        assert!(summary.contains("Input is too long"));
        assert!(summary.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD"));
    }

    #[test]
    #[cfg(not(feature = "sensitive-logs"))]
    fn test_summarize_error_body_extracts_nested_message_and_reason() {
        let body = r#"{"error":{"message":"Improperly formed request","reason":"BAD_REQUEST"}}"#;
        let summary = KiroProvider::summarize_error_body(body);
        assert!(summary.contains("Improperly formed request"));
        assert!(summary.contains("BAD_REQUEST"));
    }

    #[test]
    #[cfg(not(feature = "sensitive-logs"))]
    fn test_summarize_error_body_truncates_long_text() {
        let body = "x".repeat(1000);
        let summary = KiroProvider::summarize_error_body(&body);
        assert!(summary.len() <= 256 + 3);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_with_social_auth() {
        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("social".to_string());
        credentials.profile_arn = Some("arn:aws:sso::111111111:profile/social-profile".to_string());

        let request_body =
            r#"{"conversationState":{},"profileArn":"arn:aws:sso::999999999:profile/old"}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["profileArn"].as_str().unwrap(),
            "arn:aws:sso::111111111:profile/social-profile"
        );
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_idc_injects_credential_arn() {
        // IdC 凭据带 profileArn（如 Enterprise/Q Developer）时，必须把凭据的 profileArn 注入
        // 请求体（覆盖 body 里的旧值），否则上游 403「User is not authorized」。
        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("idc".to_string());
        credentials.profile_arn = Some("arn:aws:sso::111111111:profile/idc-profile".to_string());

        let request_body =
            r#"{"conversationState":{},"profileArn":"arn:aws:sso::999999999:profile/old"}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed.get("profileArn").and_then(|v| v.as_str()),
            Some("arn:aws:sso::111111111:profile/idc-profile")
        );
        assert!(parsed.get("conversationState").is_some());
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_builder_id_removes_field() {
        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("builder-id".to_string());

        let request_body =
            r#"{"conversationState":{},"profileArn":"arn:aws:sso::999999999:profile/old"}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("profileArn").is_none());
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_aws_sso_oidc_by_client_credentials() {
        let mut credentials = KiroCredentials::default();
        credentials.client_id = Some("client123".to_string());
        credentials.client_secret = Some("secret456".to_string());
        credentials.profile_arn = Some("arn:aws:sso::111111111:profile/test".to_string());

        let request_body =
            r#"{"conversationState":{},"profileArn":"arn:aws:sso::999999999:profile/old"}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // 凭据带 profileArn（client_id+client_secret 视为 SSO-OIDC）时注入凭据的 profileArn
        assert_eq!(
            parsed.get("profileArn").and_then(|v| v.as_str()),
            Some("arn:aws:sso::111111111:profile/test")
        );
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_without_credential_arn() {
        // 凭据无 profileArn 时，移除请求体里可能残留的脏 profileArn（profileArn 只应来自凭据）
        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("social".to_string());
        assert!(credentials.profile_arn.is_none());

        let request_body =
            r#"{"conversationState":{},"profileArn":"arn:aws:sso::999999999:profile/original"}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("profileArn").is_none());
        assert!(parsed.get("conversationState").is_some());
    }

    #[test]
    fn test_ide_endpoint_inject_profile_arn_adds_missing_field() {
        let mut credentials = KiroCredentials::default();
        credentials.auth_method = Some("social".to_string());
        credentials.profile_arn = Some("arn:aws:sso::222222222:profile/new".to_string());

        let request_body = r#"{"conversationState":{"conversationId":"test"}}"#;
        let endpoint = IdeEndpoint::new();
        let machine_id = "a".repeat(64);
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: &machine_id,
            config: &config,
        };
        let result = endpoint.transform_api_body(request_body, &ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["profileArn"].as_str().unwrap(),
            "arn:aws:sso::222222222:profile/new"
        );
        assert_eq!(
            parsed["conversationState"]["conversationId"]
                .as_str()
                .unwrap(),
            "test"
        );
    }

    #[test]
    fn test_update_default_endpoint() {
        let mut config = Config::default();
        config.default_endpoint = "ide".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.endpoint = None; // 未显式指定，应使用默认值

        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint::new()));
        endpoints.insert("cli".to_string(), Arc::new(CliEndpoint::new()));

        let tm =
            MultiTokenManager::new(config, vec![credentials.clone()], None, None, false).unwrap();
        let provider =
            KiroProvider::with_proxy(Arc::new(tm), None, endpoints.clone(), "ide".to_string());

        // 初始状态：默认 ide
        let endpoint = provider.endpoint_for(&credentials).unwrap();
        assert_eq!(endpoint.name(), "ide");

        // 热更新为 cli
        provider.update_default_endpoint("cli".to_string()).unwrap();
        let endpoint = provider.endpoint_for(&credentials).unwrap();
        assert_eq!(endpoint.name(), "cli");

        // 热更新回 ide
        provider.update_default_endpoint("ide".to_string()).unwrap();
        let endpoint = provider.endpoint_for(&credentials).unwrap();
        assert_eq!(endpoint.name(), "ide");

        // 尝试更新为未知 endpoint，应返回错误
        let result = provider.update_default_endpoint("unknown".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未知端点"));
    }

    #[test]
    fn test_endpoint_for_respects_credential_override() {
        let mut config = Config::default();
        config.default_endpoint = "ide".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.endpoint = Some("cli".to_string()); // 凭据显式指定 cli

        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint::new()));
        endpoints.insert("cli".to_string(), Arc::new(CliEndpoint::new()));

        let tm =
            MultiTokenManager::new(config, vec![credentials.clone()], None, None, false).unwrap();
        let provider = KiroProvider::with_proxy(Arc::new(tm), None, endpoints, "ide".to_string());

        // 凭据显式指定 cli，应优先使用凭据配置
        let endpoint = provider.endpoint_for(&credentials).unwrap();
        assert_eq!(endpoint.name(), "cli");

        // 即使热更新默认值为 ide，凭据显式配置仍生效
        provider.update_default_endpoint("ide".to_string()).unwrap();
        let endpoint = provider.endpoint_for(&credentials).unwrap();
        assert_eq!(endpoint.name(), "cli");
    }
}
