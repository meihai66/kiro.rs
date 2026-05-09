//! Anthropic API 中间件

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use parking_lot::RwLock;

use crate::api_key_manager::{ApiKeyManager, AuthError, AuthGuard};
use crate::common::auth;
use crate::kiro::provider::KiroProvider;
use crate::model::config::CompressionConfig;

use super::cache_tracker::CacheTracker;
use super::types::ErrorResponse;

#[derive(Clone)]
pub(crate) struct PromptCacheSnapshot {
    pub accounting_enabled: bool,
    pub ttl_seconds: u64,
    pub tracker: Arc<CacheTracker>,
}

pub struct PromptCacheRuntime {
    accounting_enabled: bool,
    ttl_seconds: u64,
    tracker: Arc<CacheTracker>,
}

impl PromptCacheRuntime {
    pub fn new(ttl_seconds: u64, accounting_enabled: bool) -> Self {
        Self {
            accounting_enabled,
            ttl_seconds,
            tracker: Arc::new(CacheTracker::new(Duration::from_secs(ttl_seconds))),
        }
    }

    pub fn snapshot(&self) -> PromptCacheSnapshot {
        PromptCacheSnapshot {
            accounting_enabled: self.accounting_enabled,
            ttl_seconds: self.ttl_seconds,
            tracker: self.tracker.clone(),
        }
    }

    pub fn update(&mut self, ttl_seconds: Option<u64>, accounting_enabled: Option<bool>) {
        if let Some(value) = accounting_enabled {
            self.accounting_enabled = value;
        }

        if let Some(value) = ttl_seconds
            && self.ttl_seconds != value
        {
            self.ttl_seconds = value;
            self.tracker = Arc::new(CacheTracker::new(Duration::from_secs(value)));
        }
    }
}

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// 启动期 seed 用的 API Key（仅启动时一次性导入 SQLite api_keys 表，运行期不用）
    #[allow(dead_code)]
    pub api_key: String,
    /// Kiro Provider（可选，用于实际 API 调用）
    /// 内部使用 MultiTokenManager，已支持线程安全的多凭据管理
    pub kiro_provider: Option<Arc<KiroProvider>>,
    /// Profile ARN（可选，用于请求）
    pub profile_arn: Option<String>,
    /// 输入压缩配置（共享引用，支持热更新）
    pub compression_config: Arc<RwLock<CompressionConfig>>,
    /// Prompt Cache 运行时配置（共享引用，支持热更新）
    pub prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    /// API Key 管理器（启动期注入；为 None 时仅用 config.api_key 兜底）
    pub api_key_manager: Option<Arc<ApiKeyManager>>,
    /// SQLite 存储（可选；用于错误日志写库）
    pub store: Option<Arc<crate::storage::Store>>,
    /// 全局配置（用于读取错误日志开关 / 黑名单等）
    pub global_config: Option<Arc<RwLock<crate::model::config::Config>>>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(
        api_key: impl Into<String>,
        prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            kiro_provider: None,
            profile_arn: None,
            compression_config: Arc::new(RwLock::new(CompressionConfig::default())),
            prompt_cache_runtime,
            api_key_manager: None,
            store: None,
            global_config: None,
        }
    }

    pub fn with_store(mut self, store: Arc<crate::storage::Store>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_global_config(
        mut self,
        config: Arc<RwLock<crate::model::config::Config>>,
    ) -> Self {
        self.global_config = Some(config);
        self
    }

    pub fn with_api_key_manager(mut self, mgr: Arc<ApiKeyManager>) -> Self {
        self.api_key_manager = Some(mgr);
        self
    }

    /// 设置 KiroProvider
    pub fn with_kiro_provider(mut self, provider: Arc<KiroProvider>) -> Self {
        self.kiro_provider = Some(provider);
        self
    }

    /// 设置 Profile ARN
    pub fn with_profile_arn(mut self, arn: impl Into<String>) -> Self {
        self.profile_arn = Some(arn.into());
        self
    }

    /// 设置压缩配置（接受共享引用）
    pub fn with_compression_config(mut self, config: Arc<RwLock<CompressionConfig>>) -> Self {
        self.compression_config = config;
        self
    }

    pub fn prompt_cache_snapshot(&self) -> PromptCacheSnapshot {
        self.prompt_cache_runtime.read().snapshot()
    }
}

/// API Key 认证中间件
///
/// 仅查 `ApiKeyManager`（数据库里的 `api_keys` 表）。`config.api_key` 在启动期会被
/// 一次性 seed 进数据库，不再作为运行期兜底比较，避免出现"双路径不可见 key"。
///
/// - 鉴权失败 → 401
/// - 并发超限 → 429 + Retry-After
/// - 成功通过 → AuthGuard 在 drop 时根据响应 status 异步写 success/fail 计数
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let key = match auth::extract_api_key(&request) {
        Some(k) => k,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::authentication_error()),
            )
                .into_response();
        }
    };

    let mgr = match &state.api_key_manager {
        Some(m) => m,
        None => {
            tracing::error!("ApiKeyManager 未注入，所有请求将被拒绝");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::authentication_error()),
            )
                .into_response();
        }
    };

    match mgr.authorize(&key) {
        Ok(guard) => {
            let guard_slot: Arc<parking_lot::Mutex<Option<AuthGuard>>> =
                Arc::new(parking_lot::Mutex::new(Some(guard)));
            let guard_held = guard_slot.clone();
            request.extensions_mut().insert(guard_slot);
            let response = next.run(request).await;
            if let Some(mut g) = guard_held.lock().take() {
                if response.status().is_success() {
                    g.mark_success();
                } else {
                    g.mark_fail();
                }
                drop(g);
            }
            response
        }
        Err(AuthError::NotFound) | Err(AuthError::Disabled) => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse::authentication_error()),
        )
            .into_response(),
        Err(AuthError::ConcurrencyLimit { limit, current }) => {
            tracing::warn!(
                api_key_name = ?mgr.get(&key).map(|e| e.name.clone()),
                limit,
                current,
                "API Key 已达并发上限"
            );
            let mut resp = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "rate_limit_error",
                        "message": format!("已达并发上限 {} (当前 {})", limit, current),
                    }
                })),
            )
                .into_response();
            resp.headers_mut()
                .insert("retry-after", "1".parse().unwrap());
            resp
        }
    }
}

/// CORS 中间件层
///
/// **安全说明**：当前配置允许所有来源（Any），这是为了支持公开 API 服务。
/// 如果需要更严格的安全控制，请根据实际需求配置具体的允许来源、方法和头信息。
///
/// # 配置说明
/// - `allow_origin(Any)`: 允许任何来源的请求
/// - `allow_methods(Any)`: 允许任何 HTTP 方法
/// - `allow_headers(Any)`: 允许任何请求头
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};

    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}
