//! Admin API 业务逻辑服务

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::anthropic::PromptCacheRuntime;
use crate::common::utf8::floor_char_boundary;
use crate::http_client::ProxyConfig;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::provider::KiroProvider;
use crate::kiro::proxy_pool::{
    AlertLevel, ProxyEntry, ProxyPool, ProxyTestResult, build_proxy_url,
    parse_host_port_user_pass_line, test_proxy,
};
use crate::kiro::proxy_rotation;
use crate::kiro::token_manager::{CachedBalanceInfo, MultiTokenManager};
use crate::model::config::{CompressionConfig, Config};
use parking_lot::RwLock;

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, BatchProxyExtendRequest,
    BatchProxyDeleteRequest, BatchProxyItemResult, BatchProxyResponse, BatchProxySlotsRequest,
    BatchProxyUnbindRequest, BindProxyRequest, CachedBalanceItem, CachedBalancesResponse,
    CredentialStatusItem, CredentialsStatusResponse, ImportAction, ImportItemResult,
    ImportProxiesRequest, ImportProxiesResponse, ImportProxyItemResult, ImportSummary,
    ImportTokenJsonRequest, ImportTokenJsonResponse, ProxyAlertItem, ProxyAlertsResponse,
    ProxyConfigResponse, ProxyEntryItem, ProxyListResponse, TokenJsonItem, TokenJsonProxyItem,
    UpdateProxyConfigRequest,
};

/// 余额缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;

/// 缓存的余额条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    kiro_provider: Option<Arc<KiroProvider>>,
    config: Arc<RwLock<Config>>,
    compression_config: Arc<RwLock<CompressionConfig>>,
    prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    cache_path: Option<PathBuf>,
    known_endpoints: HashSet<String>,
    /// 代理池（启用代理池时设置）
    proxy_pool: Option<Arc<ProxyPool>>,
    /// SQLite store（用于 RPM 历史等查询）
    store: Option<Arc<crate::storage::Store>>,
    /// API Key 管理器（启动期注入；管理员 CRUD 后调用 reload）
    api_key_manager: Option<Arc<crate::api_key_manager::ApiKeyManager>>,
}

fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        return "***".to_string();
    }
    format!("{}***{}", &key[..8], &key[key.len() - 4..])
}

fn generate_api_key() -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut bytes = [0u8; 30];
    for b in bytes.iter_mut() {
        *b = fastrand::u8(..);
    }
    let suffix = URL_SAFE_NO_PAD.encode(bytes);
    format!("sk-kiro-{}", suffix)
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        kiro_provider: Option<Arc<KiroProvider>>,
        config: Arc<RwLock<Config>>,
        compression_config: Arc<RwLock<CompressionConfig>>,
        prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
        known_endpoints: impl IntoIterator<Item = String>,
    ) -> Self {
        let cache_path = token_manager
            .cache_dir()
            .map(|d| d.join("kiro_balance_cache.json"));

        let balance_cache = Self::load_balance_cache_from(&cache_path);

        for (id, cached) in &balance_cache {
            token_manager.restore_balance_cache(*id, cached.data.remaining, cached.cached_at);
        }

        Self {
            token_manager,
            kiro_provider,
            config,
            compression_config,
            prompt_cache_runtime,
            balance_cache: Mutex::new(balance_cache),
            cache_path,
            known_endpoints: known_endpoints.into_iter().collect(),
            proxy_pool: None,
            store: None,
            api_key_manager: None,
        }
    }

    /// 注入代理池（启动期一次性）
    pub fn with_proxy_pool(mut self, pool: Arc<ProxyPool>) -> Self {
        self.proxy_pool = Some(pool);
        self
    }

    /// 注入 SQLite store（启动期一次性）
    pub fn with_store(mut self, store: Arc<crate::storage::Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// 注入 ApiKeyManager（启动期一次性）
    pub fn with_api_key_manager(
        mut self,
        mgr: Arc<crate::api_key_manager::ApiKeyManager>,
    ) -> Self {
        self.api_key_manager = Some(mgr);
        self
    }

    // ============ API Keys CRUD ============

    pub fn list_api_keys(&self) -> Result<super::types::ApiKeyListResponse, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("API Key 功能需要 SQLite store".into())
        })?;
        let rows = store
            .list_api_keys()
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let in_flight_map = self
            .api_key_manager
            .as_ref()
            .map(|m| m.snapshot_in_flight())
            .unwrap_or_default();
        let keys: Vec<super::types::ApiKeyItem> = rows
            .into_iter()
            .map(|r| {
                let mask = mask_key(&r.key);
                let in_flight = in_flight_map.get(&r.id).copied().unwrap_or(0);
                super::types::ApiKeyItem {
                    id: r.id,
                    key: r.key,
                    key_masked: mask,
                    name: r.name,
                    description: r.description,
                    enabled: r.enabled,
                    max_concurrent: r.max_concurrent,
                    cache_read_min_pct: r.cache_read_min_pct,
                    cache_read_max_pct: r.cache_read_max_pct,
                    created_at: r.created_at,
                    last_used_at: r.last_used_at,
                    success_count: r.success_count,
                    fail_count: r.fail_count,
                    in_flight,
                }
            })
            .collect();
        Ok(super::types::ApiKeyListResponse {
            total: keys.len(),
            keys,
        })
    }

    pub fn create_api_key(
        &self,
        req: super::types::CreateApiKeyRequest,
    ) -> Result<super::types::ApiKeyItem, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("API Key 功能需要 SQLite store".into())
        })?;
        if req.name.trim().is_empty() {
            return Err(AdminServiceError::InvalidRequest("name 不能为空".into()));
        }
        if req.cache_read_min_pct > 100
            || req.cache_read_max_pct > 100
            || req.cache_read_min_pct > req.cache_read_max_pct
        {
            return Err(AdminServiceError::InvalidRequest(
                "cache_read 比例必须在 [0,100] 且 min<=max".into(),
            ));
        }
        let key = req
            .custom_key
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(generate_api_key);
        let row = store
            .create_api_key(&crate::storage::ApiKeyCreate {
                key: key.clone(),
                name: req.name,
                description: req.description.filter(|s| !s.trim().is_empty()),
                enabled: true,
                max_concurrent: req.max_concurrent,
                cache_read_min_pct: req.cache_read_min_pct,
                cache_read_max_pct: req.cache_read_max_pct,
            })
            .map_err(|e| {
                if e.to_string().contains("UNIQUE") {
                    AdminServiceError::InvalidRequest("API Key 已存在".into())
                } else {
                    AdminServiceError::InternalError(e.to_string())
                }
            })?;
        if let Some(mgr) = &self.api_key_manager {
            let _ = mgr.reload();
        }
        Ok(super::types::ApiKeyItem {
            id: row.id,
            key_masked: mask_key(&row.key),
            key: row.key,
            name: row.name,
            description: row.description,
            enabled: row.enabled,
            max_concurrent: row.max_concurrent,
            cache_read_min_pct: row.cache_read_min_pct,
            cache_read_max_pct: row.cache_read_max_pct,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            success_count: row.success_count,
            fail_count: row.fail_count,
            in_flight: 0,
        })
    }

    pub fn update_api_key(
        &self,
        id: i64,
        req: super::types::UpdateApiKeyRequest,
    ) -> Result<(), AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("API Key 功能需要 SQLite store".into())
        })?;
        // 校验比例
        if let (Some(lo), Some(hi)) = (req.cache_read_min_pct, req.cache_read_max_pct)
            && (lo > 100 || hi > 100 || lo > hi)
        {
            return Err(AdminServiceError::InvalidRequest(
                "cache_read 比例必须在 [0,100] 且 min<=max".into(),
            ));
        }
        store
            .update_api_key(
                id,
                &crate::storage::ApiKeyUpdate {
                    name: req.name,
                    description: req.description,
                    enabled: req.enabled,
                    max_concurrent: req.max_concurrent,
                    cache_read_min_pct: req.cache_read_min_pct,
                    cache_read_max_pct: req.cache_read_max_pct,
                },
            )
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        if let Some(mgr) = &self.api_key_manager {
            let _ = mgr.reload();
        }
        Ok(())
    }

    pub fn delete_api_key(&self, id: i64) -> Result<(), AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("API Key 功能需要 SQLite store".into())
        })?;
        store
            .delete_api_key(id)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        if let Some(mgr) = &self.api_key_manager {
            let _ = mgr.reload();
        }
        Ok(())
    }

    /// 取过去 hours 小时的每分钟 RPM 历史
    pub fn rpm_history(
        &self,
        credential_id: u64,
        hours: i64,
    ) -> Result<Vec<(i64, u32)>, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("RPM 历史功能需要 SQLite store".into())
        })?;
        store
            .rpm_history(credential_id, hours.clamp(1, 24 * 7))
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))
    }

    /// 清空所有统计数据：
    /// - api_keys 表 success_count / fail_count 归零
    /// - 每个凭据 success_count / rate_limit_count 归零
    /// 不动：last_used_at、RPM 历史、错误日志、连续失败计数。
    pub fn reset_all_stats(&self) -> Result<u64, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("清空统计需要 SQLite store".into())
        })?;
        let api_keys_reset = store
            .reset_all_request_counts()
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.token_manager.reset_all_credential_stats();
        // 同步重新加载 ApiKeyManager 缓存（in-memory 计数）
        if let Some(mgr) = &self.api_key_manager {
            let _ = mgr.reload();
        }
        Ok(api_keys_reset)
    }

    /// 全局摘要：启动时间 + 运行时长 + 总请求次数
    pub fn stats_summary(&self) -> Result<serde_json::Value, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("统计 summary 需要 SQLite store".into())
        })?;
        let (success, fail) = store
            .aggregate_request_counts()
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let started = crate::SERVICE_STARTED_AT
            .get()
            .copied()
            .unwrap_or_else(chrono::Utc::now);
        let uptime_secs = (chrono::Utc::now() - started).num_seconds().max(0);
        Ok(serde_json::json!({
            "startedAt": started,
            "uptimeSecs": uptime_secs,
            "totalRequests": success + fail,
            "totalSuccess": success,
            "totalFail": fail,
        }))
    }

    /// 取过去 hours 小时所有凭据汇总的每分钟 RPM
    pub fn rpm_history_aggregate(
        &self,
        hours: i64,
    ) -> Result<Vec<(i64, u32)>, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("RPM 历史功能需要 SQLite store".into())
        })?;
        store
            .rpm_history_aggregate(hours.clamp(1, 24 * 7))
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))
    }

    // ============ 错误日志 ============

    /// 列表（仅返回轻量字段，request_body / response_body 不取）
    pub fn list_error_logs(
        &self,
        query: &super::types::ListErrorLogsQuery,
    ) -> Result<super::types::ErrorLogListResponse, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("错误日志功能需要 SQLite store".into())
        })?;

        let parse_csv_u16 = |s: &str| -> Vec<u16> {
            s.split(',')
                .filter_map(|t| t.trim().parse::<u16>().ok())
                .collect()
        };
        let parse_csv_str = |s: &str| -> Vec<String> {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        };
        let parse_dt = |s: &str| -> Option<chrono::DateTime<Utc>> {
            chrono::DateTime::parse_from_rfc3339(s.trim())
                .ok()
                .map(|d| d.with_timezone(&Utc))
        };

        let filter = crate::storage::ErrorLogFilter {
            status_codes: query
                .status_codes
                .as_deref()
                .map(parse_csv_u16)
                .unwrap_or_default(),
            error_kinds: query
                .error_kinds
                .as_deref()
                .map(parse_csv_str)
                .unwrap_or_default(),
            credential_id: query.credential_id,
            since: query.since.as_deref().and_then(parse_dt),
            until: query.until.as_deref().and_then(parse_dt),
        };

        let limit = query.limit.clamp(1, 500);
        let offset = query.offset;
        let (rows, total) = store
            .list_error_logs(&filter, limit, offset)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        let items: Vec<super::types::ErrorLogSummaryItem> = rows
            .into_iter()
            .map(|s| super::types::ErrorLogSummaryItem {
                id: s.id,
                at: s.at,
                credential_id: s.credential_id,
                endpoint: s.endpoint,
                status_code: s.status_code,
                upstream_status: s.upstream_status,
                error_kind: s.error_kind,
                model: s.model,
                summary: s.summary,
            })
            .collect();

        Ok(super::types::ErrorLogListResponse {
            total,
            limit,
            offset,
            items,
        })
    }

    /// 详情：单条完整内容
    pub fn get_error_log(
        &self,
        id: i64,
    ) -> Result<super::types::ErrorLogDetail, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("错误日志功能需要 SQLite store".into())
        })?;
        let row = store
            .get_error_log(id)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?
            .ok_or_else(|| AdminServiceError::NotFound { id: id as u64 })?;
        Ok(super::types::ErrorLogDetail {
            id: row.id,
            at: row.at,
            credential_id: row.credential_id,
            endpoint: row.endpoint,
            status_code: row.status_code,
            upstream_status: row.upstream_status,
            error_kind: row.error_kind,
            model: row.model,
            summary: row.summary,
            request_method: row.request_method,
            request_path: row.request_path,
            request_headers: row.request_headers,
            response_headers: row.response_headers,
            request_body: row.request_body,
            response_body: row.response_body,
            user_id: row.user_id,
            request_id: row.request_id,
        })
    }

    pub fn delete_error_log(&self, id: i64) -> Result<(), AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("错误日志功能需要 SQLite store".into())
        })?;
        let removed = store
            .delete_error_log(id)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        if !removed {
            return Err(AdminServiceError::NotFound { id: id as u64 });
        }
        Ok(())
    }

    pub fn clear_error_logs(
        &self,
        req: &super::types::ClearErrorLogsRequest,
    ) -> Result<super::types::ClearErrorLogsResponse, AdminServiceError> {
        let store = self.store.as_ref().ok_or_else(|| {
            AdminServiceError::InvalidRequest("错误日志功能需要 SQLite store".into())
        })?;
        let before = if let Some(s) = req.before.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            Some(
                chrono::DateTime::parse_from_rfc3339(s)
                    .map_err(|e| {
                        AdminServiceError::InvalidRequest(format!("before 不是合法 RFC3339: {}", e))
                    })?
                    .with_timezone(&Utc),
            )
        } else {
            None
        };
        let deleted = store
            .clear_error_logs(before)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        Ok(super::types::ClearErrorLogsResponse { deleted })
    }

    fn require_proxy_pool(&self) -> Result<Arc<ProxyPool>, AdminServiceError> {
        self.proxy_pool.clone().ok_or_else(|| {
            AdminServiceError::InvalidRequest(
                "代理池未启用（config.proxyPoolEnabled=false）".to_string(),
            )
        })
    }

    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let snapshot = self.token_manager.snapshot();

        let default_endpoint = self.config.read().default_endpoint.clone();
        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let endpoint = entry.endpoint;
                let effective_endpoint = endpoint.clone().unwrap_or(default_endpoint.clone());
                CredentialStatusItem {
                    id: entry.id,
                    priority: entry.priority,
                    disabled: entry.disabled,
                    failure_count: entry.failure_count,
                    refresh_failure_count: entry.refresh_failure_count,
                    disabled_reason: entry.disable_reason.map(|reason| format!("{:?}", reason)),
                    expires_at: entry.expires_at,
                    auth_method: entry.auth_method,
                    has_profile_arn: entry.has_profile_arn,
                    refresh_token_hash: entry.refresh_token_hash,
                    email: entry.email,
                    subscription_title: entry.subscription_title,
                    success_count: entry.success_count,
                    last_used_at: entry.last_used_at.clone(),
                    region: entry.region,
                    api_region: entry.api_region,
                    endpoint,
                    effective_endpoint,
                    proxy_slot_id: entry.proxy_slot_id,
                    overage_status: entry.overage_status,
                    in_flight: entry.in_flight,
                    rpm: entry.rpm,
                    rate_limit_count: entry.rate_limit_count,
                    allow_overuse: entry.allow_overuse,
                    cooldown_reason: entry.cooldown_reason,
                    cooldown_remaining_secs: entry.cooldown_remaining_secs,
                    credential_rpm: entry.credential_rpm,
                }
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        CredentialsStatusResponse {
            total: snapshot.total,
            available: snapshot.available,
            credentials,
        }
    }

    /// 设置凭据禁用状态
    ///
    /// 启用代理池后，启用凭据需要先绑定代理槽（防止回退本地直连）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> Result<(), AdminServiceError> {
        if !disabled && self.proxy_pool.is_some() {
            // 启用前校验是否已绑定代理槽
            let snapshot = self.token_manager.snapshot();
            let entry = snapshot
                .entries
                .iter()
                .find(|e| e.id == id)
                .ok_or(AdminServiceError::NotFound { id })?;
            if entry.proxy_slot_id.is_none() {
                return Err(AdminServiceError::InvalidCredential(
                    "凭据未绑定代理槽，禁止启用（请先调用 bind-proxy）".to_string(),
                ));
            }
        }
        self.token_manager
            .set_disabled(id, disabled)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据 Region
    pub fn set_region(
        &self,
        id: u64,
        region: Option<String>,
        api_region: Option<String>,
    ) -> Result<(), AdminServiceError> {
        // trim 后空字符串转 None
        let region = region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.token_manager
            .set_region(id, region, api_region)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据「允许超额使用」开关
    pub fn set_allow_overuse(&self, id: u64, allow: bool) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_allow_overuse(id, allow)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置单凭据 RPM 上限（None 表示清除覆盖，沿用全局 credentialRpm）
    pub fn set_credential_rpm(
        &self,
        id: u64,
        rpm: Option<u32>,
    ) -> Result<(), AdminServiceError> {
        let normalized = rpm.filter(|&v| v > 0);
        self.token_manager
            .set_credential_rpm_for(id, normalized)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据邮箱
    pub fn set_email(&self, id: u64, email: Option<String>) -> Result<(), AdminServiceError> {
        let email = email
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.token_manager
            .set_email(id, email)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据 endpoint
    pub fn set_endpoint(&self, id: u64, endpoint: Option<String>) -> Result<(), AdminServiceError> {
        let endpoint = endpoint
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(name) = endpoint.as_deref()
            && !self.known_endpoints.contains(name)
        {
            let mut known: Vec<&str> = self.known_endpoints.iter().map(|s| s.as_str()).collect();
            known.sort_unstable();
            return Err(AdminServiceError::InvalidCredential(format!(
                "endpoint 必须是已注册值，已注册: {:?}，收到: {}",
                known, name
            )));
        }

        self.token_manager
            .set_endpoint(id, endpoint)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 强制刷新指定凭据 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_error(e, id))
    }

    /// 列出指定凭据支持的模型（调上游 ListAvailableModels）
    pub async fn list_credential_models(
        &self,
        id: u64,
    ) -> Result<serde_json::Value, AdminServiceError> {
        self.token_manager
            .list_available_models_for(id)
            .await
            .map_err(|e| AdminServiceError::UpstreamError(e.to_string()))
    }

    /// 设置上游 overage 偏好（开关超额计费）
    ///
    /// 与上游 Go 实现一致：上游成功 → 返回 ok:true；上游失败 → 返回 ok:false + error 字段（HTTP 仍 200）
    pub async fn set_overage_preference(
        &self,
        id: u64,
        status: &str,
    ) -> super::types::SetOveragePreferenceResponse {
        match self.token_manager.set_overage_for(id, status).await {
            Ok(s) => super::types::SetOveragePreferenceResponse {
                ok: true,
                overage_status: Some(s),
                error: None,
            },
            Err(e) => super::types::SetOveragePreferenceResponse {
                ok: false,
                overage_status: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// 获取凭据余额（带缓存）
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        // 先查缓存
        {
            let cache = self.balance_cache.lock();
            if let Some(cached) = cache.get(&id) {
                let now = Utc::now().timestamp() as f64;
                if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    tracing::debug!("凭据 #{} 余额命中缓存", id);
                    return Ok(cached.data.clone());
                }
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = self.fetch_balance(id).await?;

        // 更新缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();

        Ok(balance)
    }

    /// 强制从上游刷新余额，绕过 5min AdminService 缓存。
    /// 用于后台周期刷新任务；前端"查余额"仍走 get_balance（带 5min 缓存）。
    pub async fn refresh_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let balance = self.fetch_balance(id).await?;
        // 同时更新 AdminService 内存缓存，让 get_cached_balances 立即看到新值
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();
        Ok(balance)
    }

    /// 取出当前缓存的余额年龄（秒），用于后台任务判断是否需要刷新。
    /// 返回 None 表示没缓存（视为"特别旧"）。
    pub fn balance_cache_age_secs(&self, id: u64) -> Option<u64> {
        let cache = self.balance_cache.lock();
        let cached = cache.get(&id)?;
        let now = Utc::now().timestamp() as f64;
        Some(((now - cached.cached_at).max(0.0)) as u64)
    }

    /// 从上游获取余额（无缓存）
    async fn fetch_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let usage = self
            .token_manager
            .get_usage_limits_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        let current_usage = usage.current_usage();
        let usage_limit = usage.usage_limit();
        // 不再 clamp，让超额体现为 remaining < 0；前端据此显示"超 +N"
        let remaining = usage_limit - current_usage;
        let usage_percentage = if usage_limit > 0.0 {
            // 不再 clamp 100%，超额显示真实百分比
            current_usage / usage_limit * 100.0
        } else {
            0.0
        };

        // 更新缓存，使列表页面能显示最新余额
        self.token_manager.update_balance_cache(id, remaining);

        // 使用率自动禁用（config.auto_disable_usage_threshold_pct）
        let threshold = self.config.read().auto_disable_usage_threshold_pct;
        if threshold > 0 && usage_percentage >= threshold as f64 {
            tracing::warn!(
                credential_id = id,
                usage_pct = %format!("{:.1}", usage_percentage),
                threshold_pct = threshold,
                "凭据使用率超过自动禁用阈值，已禁用"
            );
            self.token_manager.mark_insufficient_balance(id);
        }

        Ok(BalanceResponse {
            id,
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
        })
    }

    /// 获取所有凭据的缓存余额
    pub fn get_cached_balances(&self) -> CachedBalancesResponse {
        // 从 token_manager 获取运行时缓存（含 TTL 信息）
        let runtime_balances: HashMap<u64, CachedBalanceInfo> = self
            .token_manager
            .get_all_cached_balances()
            .into_iter()
            .map(|info| (info.id, info))
            .collect();

        // 从 AdminService 磁盘缓存获取完整余额信息
        let disk_cache = self.balance_cache.lock();

        let balances = runtime_balances
            .into_iter()
            .map(|(id, info)| {
                // 优先从磁盘缓存获取完整快照（保证字段一致性）
                if let Some(cached) = disk_cache.get(&id) {
                    CachedBalanceItem {
                        id,
                        remaining: cached.data.remaining,
                        usage_limit: cached.data.usage_limit,
                        usage_percentage: cached.data.usage_percentage,
                        subscription_title: cached.data.subscription_title.clone(),
                        cached_at: info.cached_at,
                        ttl_secs: info.ttl_secs,
                    }
                } else {
                    CachedBalanceItem {
                        id,
                        remaining: info.remaining,
                        usage_limit: 0.0,
                        usage_percentage: 0.0,
                        subscription_title: None,
                        cached_at: info.cached_at,
                        ttl_secs: info.ttl_secs,
                    }
                }
            })
            .collect();

        CachedBalancesResponse { balances }
    }

    /// 添加新凭据
    ///
    /// 启用代理池时：
    /// - `auto_bind_proxy=true` 且未指定 `proxy_slot_id`：从池里选最优空闲代理绑定；
    ///   找不到则返回 409 拒绝（不允许悬挂）。
    /// - `auto_bind_proxy=true` 且指定了 `proxy_slot_id`：尝试绑到指定槽。
    /// - `auto_bind_proxy=false`：凭据导入后置 `disabled=true`，等待手动 bind。
    pub async fn add_credential(
        &self,
        req: AddCredentialRequest,
    ) -> Result<AddCredentialResponse, AdminServiceError> {
        // 构建凭据对象
        let email = req.email.clone();
        let effective_auth_method = if req
            .kiro_api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            "api_key".to_string()
        } else {
            req.auth_method.clone()
        };
        let endpoint = req
            .endpoint
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(name) = endpoint.as_deref()
            && !self.known_endpoints.contains(name)
        {
            let mut known: Vec<&str> = self.known_endpoints.iter().map(|s| s.as_str()).collect();
            known.sort_unstable();
            return Err(AdminServiceError::InvalidCredential(format!(
                "endpoint 必须是已注册值，已注册: {:?}，收到: {}",
                known, name
            )));
        }

        // ===== 启用代理池时的预绑定逻辑 =====
        // 关键：必须在 token_manager.add_credential（内部会做 refresh / getUsageLimits 上游调用）
        // **之前** 给 new_cred 写好 proxy_slot_id，否则验证那步走代理时会 fail。
        let auto_bind = req.auto_bind_proxy.unwrap_or(true);
        let warning_hours = self.config.read().proxy_expiry_warning_hours;
        let mut prepick_slot: Option<String> = None;
        let mut should_disable_after_create = false;

        if let Some(pool) = &self.proxy_pool {
            if let Some(explicit_slot) = req.proxy_slot_id.as_deref() {
                // 用户显式指定代理槽 — 先校验存在 + 仍有空槽
                let entry = pool.get(explicit_slot).ok_or_else(|| {
                    AdminServiceError::InvalidRequest(format!(
                        "指定的代理槽 {} 不存在",
                        explicit_slot
                    ))
                })?;
                if !entry.has_free_slot() {
                    return Err(AdminServiceError::InvalidRequest(format!(
                        "代理槽 {} 已无空闲位（{}/{}）",
                        explicit_slot,
                        entry.used_slots(),
                        entry.slots
                    )));
                }
                prepick_slot = Some(explicit_slot.to_string());
            } else if auto_bind {
                // 仅 pick（不修改池状态），把 slot 提前写进 new_cred 让验证阶段能用代理
                prepick_slot = pool.pick_idle_candidate(warning_hours);
                if prepick_slot.is_none() {
                    return Err(AdminServiceError::InvalidRequest(
                        "代理池无可用代理（剩余槽位 + 有效期充足），请先导入代理或减少阈值".to_string(),
                    ));
                }
            } else {
                // 不自动绑定 → 凭据建好后强制禁用（不能在没有代理槽时启用）
                should_disable_after_create = true;
            }
        }

        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: req.refresh_token,
            kiro_api_key: req.kiro_api_key,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(effective_auth_method),
            client_id: req.client_id,
            client_secret: req.client_secret,
            priority: req.priority,
            region: req.region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            endpoint,
            email: req.email,
            subscription_title: None,
            proxy_slot_id: prepick_slot.clone(),
            disabled: false,
            allow_overuse: false,
            rpm: None,
            last_overage_status: None,
            runtime_only: false,
        };

        // 调用 token_manager 添加凭据（验证阶段会走 prepick_slot 对应的代理）
        let credential_id = self
            .token_manager
            .add_credential(new_cred)
            .await
            .map_err(|e| self.classify_add_error(e))?;

        // ===== 凭据建好后，把代理槽真正绑上去 =====
        if let Some(pool) = &self.proxy_pool {
            if let Some(slot_id) = prepick_slot {
                // manual_bind 是幂等的，把 credential_id push 进 boundCredentialIds
                if let Err(e) = pool.manual_bind(&slot_id, credential_id) {
                    // 极少发生：并发导入挤占同一槽位
                    tracing::warn!(
                        credential_id,
                        slot_id = %slot_id,
                        "凭据创建后绑定代理槽失败（可能并发抢占），尝试重新自动选槽: {}",
                        e
                    );
                    match pool.auto_bind(credential_id, warning_hours) {
                        Ok(other) => {
                            let _ =
                                self.token_manager.set_proxy_slot(credential_id, Some(other));
                        }
                        Err(e2) => {
                            pool.push_alert(
                                AlertLevel::Error,
                                format!(
                                    "凭据 #{} 创建成功但绑定代理失败（{}），已置 disabled",
                                    credential_id, e2
                                ),
                            );
                            let _ = self.token_manager.set_proxy_slot(credential_id, None);
                            let _ = self.token_manager.set_disabled(credential_id, true);
                        }
                    }
                }
            }
            if should_disable_after_create {
                let _ = self.token_manager.set_proxy_slot(credential_id, None);
                let _ = self.token_manager.set_disabled(credential_id, true);
            }
        }

        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }

        let message = if should_disable_after_create {
            format!(
                "凭据添加成功（ID: {}），未自动绑定代理，已置 disabled=true，请手动绑定代理后再启用",
                credential_id
            )
        } else {
            format!("凭据添加成功，ID: {}", credential_id)
        };

        Ok(AddCredentialResponse {
            success: true,
            message,
            credential_id,
            email,
        })
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;

        // 清理已删除凭据的余额缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.remove(&id);
        }
        self.save_balance_cache();

        // 同步从代理池解绑
        if let Some(pool) = &self.proxy_pool {
            let affected = pool.unbind_credential(id);
            if !affected.is_empty() {
                tracing::info!(
                    credential_id = id,
                    affected_proxies = ?affected,
                    "凭据删除：已自动从代理池解绑"
                );
            }
        }

        Ok(())
    }

    // ============ 余额缓存持久化 ============

    fn load_balance_cache_from(cache_path: &Option<PathBuf>) -> HashMap<u64, CachedBalance> {
        let path = match cache_path {
            Some(p) => p,
            None => return HashMap::new(),
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };

        // 文件中使用字符串 key 以兼容 JSON 格式
        let map: HashMap<String, CachedBalance> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解析余额缓存失败，将忽略: {}", e);
                return HashMap::new();
            }
        };

        // 加载所有持久化的余额条目，不在加载时丢弃过期数据。
        // 运行期 token_manager 会按动态 TTL（高频/低频/低余额）异步刷新；
        // 即使旧数据，前端也能看到上次查询的快照（带 cached_at 时间戳），
        // 避免重启或长时间空闲后页面表格里余额"消失"的现象。
        map.into_iter()
            .filter_map(|(k, v)| {
                let id = k.parse::<u64>().ok()?;
                Some((id, v))
            })
            .collect()
    }

    fn save_balance_cache(&self) {
        let path = match &self.cache_path {
            Some(p) => p,
            None => return,
        };

        // 快速 clone 数据后释放锁，减少锁持有时间
        let map: HashMap<String, CachedBalance> = {
            let cache = self.balance_cache.lock();
            cache
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect()
        };

        // 锁外执行序列化和文件 IO
        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                // 原子写入：先写临时文件，再重命名
                let tmp_path = path.with_extension("json.tmp");
                match std::fs::write(&tmp_path, json) {
                    Ok(_) => {
                        if let Err(e) = std::fs::rename(&tmp_path, path) {
                            tracing::warn!("原子重命名余额缓存失败: {}", e);
                            let _ = std::fs::remove_file(&tmp_path);
                        }
                    }
                    Err(e) => tracing::warn!("写入临时余额文件失败: {}", e),
                }
            }
            Err(e) => tracing::warn!("序列化余额缓存失败: {}", e),
        }
    }

    // ============ 错误分类 ============

    /// 分类简单操作错误（set_disabled, set_priority, reset_and_enable）
    fn classify_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("API Key 凭据无需刷新 Token") {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类余额查询错误（可能涉及上游 API 调用）
    fn classify_balance_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();

        // 1. 凭据不存在
        if msg.contains("不存在") {
            return AdminServiceError::NotFound { id };
        }

        // 2. 上游服务错误特征：HTTP 响应错误或网络错误
        let is_upstream_error =
            // HTTP 响应错误（来自 refresh_*_token 的错误消息）
            msg.contains("凭证已过期或无效") ||
            msg.contains("权限不足") ||
            msg.contains("已被限流") ||
            msg.contains("服务器错误") ||
            msg.contains("Token 刷新失败") ||
            msg.contains("暂时不可用") ||
            // 网络错误（reqwest 错误）
            msg.contains("error trying to connect") ||
            msg.contains("connection") ||
            msg.contains("timeout") ||
            msg.contains("timed out");

        if is_upstream_error {
            AdminServiceError::UpstreamError(msg)
        } else {
            // 3. 默认归类为内部错误（本地验证失败、配置错误等）
            // 包括：缺少 refreshToken、refreshToken 已被截断、无法生成 machineId 等
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类添加凭据错误
    fn classify_add_error(&self, e: anyhow::Error) -> AdminServiceError {
        let msg = e.to_string();

        // 凭据验证失败（refreshToken 无效、格式错误等）
        let is_invalid_credential = msg.contains("缺少 refreshToken")
            || msg.contains("refreshToken 为空")
            || msg.contains("refreshToken 已被截断")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("kiroApiKey 为空")
            || msg.contains("API Key 凭据无需刷新 Token")
            || msg.contains("凭据已存在")
            || msg.contains("refreshToken 或 kiroApiKey 重复")
            || msg.contains("凭证已过期或无效")
            || msg.contains("认证失败")
            || msg.contains("权限不足")
            || msg.contains("已被限流");

        if is_invalid_credential {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类删除凭据错误
    fn classify_delete_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("只能删除已禁用的凭据") || msg.contains("请先禁用凭据")
        {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 批量导入 token.json
    ///
    /// 解析官方 token.json 格式，按 provider 字段自动映射 authMethod：
    /// - BuilderId/builder-id/idc → idc
    /// - Social/social → social
    pub async fn import_token_json(&self, req: ImportTokenJsonRequest) -> ImportTokenJsonResponse {
        let items = req.items.into_vec();
        let dry_run = req.dry_run;

        let mut results = Vec::with_capacity(items.len());
        let mut added = 0usize;
        let mut skipped = 0usize;
        let mut invalid = 0usize;

        for (index, item) in items.into_iter().enumerate() {
            let result = self.process_token_json_item(index, item, dry_run).await;
            match result.action {
                ImportAction::Added => added += 1,
                ImportAction::Skipped => skipped += 1,
                ImportAction::Invalid => invalid += 1,
            }
            results.push(result);
        }

        ImportTokenJsonResponse {
            summary: ImportSummary {
                parsed: results.len(),
                added,
                skipped,
                invalid,
            },
            items: results,
        }
    }

    /// 处理单个 token.json 项
    async fn process_token_json_item(
        &self,
        index: usize,
        item: TokenJsonItem,
        dry_run: bool,
    ) -> ImportItemResult {
        // 生成指纹（用于识别和去重）
        let fingerprint = Self::generate_fingerprint(&item);

        // 验证必填字段
        let refresh_token = match &item.refresh_token {
            Some(rt) if !rt.is_empty() => rt.clone(),
            _ => {
                return ImportItemResult {
                    index,
                    fingerprint,
                    action: ImportAction::Invalid,
                    reason: Some("缺少 refreshToken".to_string()),
                    credential_id: None,
                };
            }
        };

        // 映射 authMethod
        let auth_method = Self::map_auth_method(&item);

        // IdC 需要 clientId 和 clientSecret
        if auth_method == "idc" && (item.client_id.is_none() || item.client_secret.is_none()) {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Invalid,
                reason: Some(format!("{} 认证需要 clientId 和 clientSecret", auth_method)),
                credential_id: None,
            };
        }

        // 检查是否已存在（通过 refreshToken 前缀匹配）
        if self.token_manager.has_refresh_token_prefix(&refresh_token) {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Skipped,
                reason: Some("凭据已存在".to_string()),
                credential_id: None,
            };
        }

        // dry-run 模式只返回预览
        if dry_run {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Added,
                reason: Some("预览模式".to_string()),
                credential_id: None,
            };
        }

        // 实际添加凭据（trim + 空字符串转 None，与 set_region 逻辑一致）
        let region = item
            .region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = item
            .api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let warning_hours = self.config.read().proxy_expiry_warning_hours;

        // 内嵌代理：先把代理加入池（占好槽），让 add_credential 的代理校验能通过；
        // 之后 add_credential 成功再 manual_bind 落实占用。
        let embedded_proxy = item.proxy.clone();
        tracing::info!(
            index,
            has_embedded_proxy = embedded_proxy.is_some(),
            embedded_proxy_url = embedded_proxy.as_ref().map(|p| p.url.as_str()),
            pool_enabled = self.proxy_pool.is_some(),
            "import-token-json: 解析单项"
        );
        let embedded_slot_id: Option<String> = if let Some(p) = embedded_proxy.as_ref() {
            match self.ensure_proxy_in_pool(p) {
                Ok(id) => {
                    tracing::info!(
                        index,
                        proxy_id = %id,
                        "import-token-json: 内嵌代理已加入/复用代理池"
                    );
                    Some(id)
                }
                Err(e) => {
                    tracing::warn!(
                        index,
                        proxy_url = %p.url,
                        error = %e,
                        "import-token-json: 内嵌代理加入池失败"
                    );
                    return ImportItemResult {
                        index,
                        fingerprint,
                        action: ImportAction::Invalid,
                        reason: Some(format!("内嵌代理加入池失败：{}", e)),
                        credential_id: None,
                    };
                }
            }
        } else {
            None
        };

        // 既有路径：池启用且无内嵌代理 → 走 prepick + auto_bind
        let prepick_slot: Option<String> = if embedded_slot_id.is_some() {
            embedded_slot_id.clone()
        } else {
            self.proxy_pool
                .as_ref()
                .and_then(|pool| pool.pick_idle_candidate(warning_hours))
        };

        // 如果代理池启用但拿不到 slot（既无内嵌也无可用 idle），直接报错给前端，
        // 避免下游 add_credential 抛出意义模糊的「凭据 #None 未绑定代理槽」。
        if self.proxy_pool.is_some() && prepick_slot.is_none() {
            return ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Invalid,
                reason: Some(
                    "代理池启用但无可用代理槽：请在导入文件中带上 proxy 字段，或先向代理池添加代理"
                        .to_string(),
                ),
                credential_id: None,
            };
        }

        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: Some(refresh_token),
            kiro_api_key: None,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(auth_method),
            client_id: item.client_id,
            client_secret: item.client_secret,
            priority: item.priority,
            region,
            api_region,
            machine_id: item.machine_id,
            endpoint: None,
            email: item
                .email
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            subscription_title: None,
            proxy_slot_id: prepick_slot.clone(),
            disabled: false,
            allow_overuse: false,
            rpm: None,
            last_overage_status: None,
            runtime_only: false,
        };

        match self.token_manager.add_credential(new_cred).await {
            Ok(credential_id) => {
                let mut reason: Option<String> = None;
                if let Some(slot_id) = embedded_slot_id.as_deref() {
                    // 内嵌代理：强制绑定到我们刚加入的槽
                    if let Some(pool) = &self.proxy_pool {
                        match pool.manual_bind(slot_id, credential_id) {
                            Ok(()) => {
                                let _ = self
                                    .token_manager
                                    .set_proxy_slot(credential_id, Some(slot_id.to_string()));
                                tracing::info!(
                                    credential_id,
                                    slot_id,
                                    "批量导入：内嵌代理已绑定"
                                );
                            }
                            Err(e) => {
                                pool.push_alert(
                                    AlertLevel::Warn,
                                    format!(
                                        "批量导入：凭据 #{} 内嵌代理 {} 绑定失败（{}），已置 disabled",
                                        credential_id, slot_id, e
                                    ),
                                );
                                let _ = self.token_manager.set_proxy_slot(credential_id, None);
                                let _ = self.token_manager.set_disabled(credential_id, true);
                                reason = Some(format!("内嵌代理绑定失败（{}），已置 disabled", e));
                            }
                        }
                    }
                } else if let Some(pool) = &self.proxy_pool {
                    // 真正占槽：先尝试 prepick 的，若并发被抢占则再 auto_bind 一次
                    let bound = match prepick_slot.as_deref() {
                        Some(slot) => pool
                            .manual_bind(slot, credential_id)
                            .map(|()| slot.to_string())
                            .or_else(|_| pool.auto_bind(credential_id, warning_hours)),
                        None => pool.auto_bind(credential_id, warning_hours),
                    };
                    match bound {
                        Ok(slot) => {
                            let _ = self
                                .token_manager
                                .set_proxy_slot(credential_id, Some(slot));
                        }
                        Err(e) => {
                            tracing::warn!(
                                credential_id,
                                "批量导入：绑定代理失败，凭据置 disabled: {}",
                                e
                            );
                            pool.push_alert(
                                AlertLevel::Warn,
                                format!(
                                    "批量导入：凭据 #{} 绑定代理失败（{}），已置 disabled",
                                    credential_id, e
                                ),
                            );
                            let _ = self.token_manager.set_proxy_slot(credential_id, None);
                            let _ = self.token_manager.set_disabled(credential_id, true);
                            reason = Some(format!("已添加但未绑定代理（{}），已置 disabled", e));
                        }
                    }
                }
                ImportItemResult {
                    index,
                    fingerprint,
                    action: ImportAction::Added,
                    reason,
                    credential_id: Some(credential_id),
                }
            }
            Err(e) => ImportItemResult {
                index,
                fingerprint,
                action: ImportAction::Invalid,
                reason: Some(e.to_string()),
                credential_id: None,
            },
        }
    }

    /// 确保内嵌代理在代理池中存在（占好槽，未绑定凭据），返回该代理 ID。
    ///
    /// 行为：
    /// - 解析 URL 中的 user:pass，分离为干净 URL + 独立的 username/password
    /// - **不去重**：每次调用都新建一条独立条目（slots=1，到期取自传入或默认 30 天后）。
    ///   即便同一 host:port 已在池里，也再加一条——这样多个凭据带相同代理时各自有独立 slot，
    ///   不会因 slots=1 互抢。
    ///
    /// 不在此处绑定凭据；调用方负责 add_credential 成功后再 manual_bind。
    fn ensure_proxy_in_pool(
        &self,
        proxy_info: &TokenJsonProxyItem,
    ) -> Result<String, AdminServiceError> {
        let pool = self
            .proxy_pool
            .as_ref()
            .ok_or_else(|| AdminServiceError::InvalidRequest("代理池未启用".to_string()))?;

        let raw_url = proxy_info.url.trim();
        if raw_url.is_empty() {
            return Err(AdminServiceError::InvalidRequest("代理 URL 为空".to_string()));
        }
        let (clean_url, username, password) = parse_proxy_url_with_auth(raw_url)
            .map_err(|e| AdminServiceError::InvalidRequest(format!("代理 URL 无效: {}", e)))?;
        let expires_at = parse_proxy_expires_at(proxy_info.expires_at.as_ref());

        let entry = ProxyEntry {
            id: String::new(),
            url: clean_url,
            username,
            password,
            expires_at,
            slots: 1,
            bound_credential_ids: vec![],
            label: proxy_info
                .label
                .clone()
                .or_else(|| Some("imported".to_string())),
            created_at: Utc::now(),
            last_rotated_at: None,
        };
        let mut ids = pool.add_many_force(vec![entry]);
        ids.pop()
            .ok_or_else(|| AdminServiceError::InternalError("add_many_force 未返回 id".to_string()))
    }

    /// 生成凭据指纹（用于识别）
    fn generate_fingerprint(item: &TokenJsonItem) -> String {
        // 使用 refreshToken 前 16 字符作为指纹
        // 使用 floor_char_boundary 安全截断，避免在多字节字符中间切割导致 panic
        item.refresh_token
            .as_ref()
            .map(|rt| {
                if rt.len() >= 16 {
                    let end = floor_char_boundary(rt, 16);
                    format!("{}...", &rt[..end])
                } else {
                    rt.clone()
                }
            })
            .unwrap_or_else(|| "(empty)".to_string())
    }

    /// 映射 provider/authMethod 到标准 authMethod
    fn map_auth_method(item: &TokenJsonItem) -> String {
        // 优先使用 authMethod 字段
        if let Some(auth) = &item.auth_method {
            let auth_lower = auth.to_lowercase();
            return match auth_lower.as_str() {
                "idc" | "builder-id" | "builderid" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => auth_lower,
            };
        }

        // 回退到 provider 字段
        if let Some(provider) = &item.provider {
            let provider_lower = provider.to_lowercase();
            return match provider_lower.as_str() {
                "builderid" | "builder-id" | "idc" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => "social".to_string(), // 默认 social
            };
        }

        // 默认 social
        "social".to_string()
    }

    /// 获取当前代理配置（脱敏）
    pub fn get_proxy_config(&self) -> ProxyConfigResponse {
        let config = self.config.read();
        ProxyConfigResponse {
            proxy_url: config.proxy_url.clone(),
            has_credentials: config.proxy_username.is_some() && config.proxy_password.is_some(),
        }
    }

    /// 更新代理配置（热更新）
    pub async fn update_proxy_config(
        &self,
        req: UpdateProxyConfigRequest,
    ) -> Result<(), AdminServiceError> {
        // 1. 构建新的 ProxyConfig
        let new_proxy = if let Some(url) = &req.proxy_url {
            if url.trim().is_empty() {
                None
            } else {
                let mut proxy = ProxyConfig::new(url.trim());
                if let (Some(u), Some(p)) = (&req.proxy_username, &req.proxy_password)
                    && !u.trim().is_empty()
                    && !p.trim().is_empty()
                {
                    proxy = proxy.with_auth(u.trim(), p.trim());
                }
                // 如果未提供新认证信息，保留现有认证
                if proxy.username.is_none() {
                    let config = self.config.read();
                    if let (Some(u), Some(p)) = (&config.proxy_username, &config.proxy_password) {
                        proxy = proxy.with_auth(u, p);
                    }
                }
                Some(proxy)
            }
        } else {
            None
        };

        // 2. 先持久化配置（失败时不影响运行时状态）
        {
            let mut config = self.config.write();
            config.proxy_url = new_proxy.as_ref().map(|p| p.url.clone());
            config.proxy_username = new_proxy.as_ref().and_then(|p| p.username.clone());
            config.proxy_password = new_proxy.as_ref().and_then(|p| p.password.clone());
            config
                .save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        // 3. 持久化成功后再应用运行时变更
        if let Some(provider) = &self.kiro_provider {
            provider
                .update_global_proxy(new_proxy.clone())
                .map_err(|e| AdminServiceError::InternalError(format!("代理配置无效: {}", e)))?;
        }

        // 4. 热更新 MultiTokenManager
        self.token_manager.update_proxy(new_proxy.clone());

        // 5. 同步更新 count_tokens 通道的代理配置
        crate::token::update_proxy(new_proxy);

        Ok(())
    }

    /// 获取全局配置
    pub fn get_global_config(&self) -> super::types::GlobalConfigResponse {
        let config = self.config.read();
        let c = self.compression_config.read();
        super::types::GlobalConfigResponse {
            region: config.region.clone(),
            credential_rpm: config.credential_rpm,
            prompt_cache_ttl_seconds: config.prompt_cache_ttl_seconds,
            prompt_cache_accounting_enabled: config.prompt_cache_accounting_enabled,
            default_endpoint: config.default_endpoint.clone(),
            compression: super::types::CompressionConfigResponse {
                enabled: c.enabled,
                whitespace_compression: c.whitespace_compression,
                thinking_strategy: c.thinking_strategy.clone(),
                tool_result_max_chars: c.tool_result_max_chars,
                tool_result_head_lines: c.tool_result_head_lines,
                tool_result_tail_lines: c.tool_result_tail_lines,
                tool_use_input_max_chars: c.tool_use_input_max_chars,
                tool_description_max_chars: c.tool_description_max_chars,
                max_history_turns: c.max_history_turns,
                max_history_chars: c.max_history_chars,
                max_request_body_bytes: c.max_request_body_bytes,
            },
            auto_disable_patterns: config.auto_disable_patterns.clone(),
            error_replace_rules: config.error_replace_rules.clone(),
            auto_disable_usage_threshold_pct: config.auto_disable_usage_threshold_pct,
            max_retries_per_credential: config.max_retries_per_credential,
            max_total_retries: config.max_total_retries,
            all_credentials_cooldown_bail_threshold_secs: config
                .all_credentials_cooldown_bail_threshold_secs,
            balance_auto_refresh_secs: config.balance_auto_refresh_secs,
            error_log_enabled: config.error_log_enabled,
            error_log_max_count: config.error_log_max_count,
            error_log_max_age_days: config.error_log_max_age_days,
            error_log_excluded_status_codes: config.error_log_excluded_status_codes.clone(),
        }
    }

    /// 更新全局配置
    pub async fn update_global_config(
        &self,
        req: super::types::UpdateGlobalConfigRequest,
    ) -> Result<(), AdminServiceError> {
        // 1. 先持久化配置（失败时不影响运行时状态）
        {
            let mut config = self.config.write();

            if let Some(region) = &req.region {
                let trimmed = region.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidRequest(
                        "Region 不能为空".to_string(),
                    ));
                }
                config.region = trimmed.to_string();
            }

            if let Some(rpm) = req.credential_rpm {
                config.credential_rpm = rpm;
            }

            if let Some(ttl_seconds) = req.prompt_cache_ttl_seconds {
                if !matches!(ttl_seconds, 300 | 3600) {
                    return Err(AdminServiceError::InvalidRequest(
                        "Prompt Cache TTL 仅支持 300（5分钟）或 3600（1小时）".to_string(),
                    ));
                }
                config.prompt_cache_ttl_seconds = ttl_seconds;
            }

            if let Some(enabled) = req.prompt_cache_accounting_enabled {
                config.prompt_cache_accounting_enabled = enabled;
            }

            if let Some(ref endpoint) = req.default_endpoint {
                let trimmed = endpoint.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidRequest(
                        "默认 endpoint 不能为空".to_string(),
                    ));
                }
                if !self.known_endpoints.contains(trimmed) {
                    return Err(AdminServiceError::InvalidRequest(format!(
                        "未知的 endpoint: {}，可用值: {}",
                        trimmed,
                        self.known_endpoints
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                config.default_endpoint = trimmed.to_string();
            }

            if let Some(c) = &req.compression {
                Self::apply_compression_fields(&mut config.compression, c);
            }

            if let Some(patterns) = &req.auto_disable_patterns {
                // 整体替换：去重、trim、过滤空行
                let mut cleaned: Vec<String> = patterns
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                cleaned.sort();
                cleaned.dedup();
                config.auto_disable_patterns = cleaned;
            }

            if let Some(rules) = &req.error_replace_rules {
                // 保留顺序（不去重排序：先到先匹配，便于用户控制优先级）；仅 trim 行内空白 + 去掉空行
                let cleaned: Vec<String> = rules
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                config.error_replace_rules = cleaned;
            }

            if let Some(pct) = req.auto_disable_usage_threshold_pct {
                if pct > 100 {
                    return Err(AdminServiceError::InvalidRequest(
                        "使用率阈值必须在 0~100".into(),
                    ));
                }
                config.auto_disable_usage_threshold_pct = pct;
            }

            if let Some(v) = req.max_retries_per_credential {
                if v > 100 {
                    return Err(AdminServiceError::InvalidRequest(
                        "单凭据重试次数过大（>100）".into(),
                    ));
                }
                config.max_retries_per_credential = v;
            }

            if let Some(v) = req.max_total_retries {
                if v > 100 {
                    return Err(AdminServiceError::InvalidRequest(
                        "单请求总重试次数过大（>100）".into(),
                    ));
                }
                config.max_total_retries = v;
            }

            if let Some(v) = req.all_credentials_cooldown_bail_threshold_secs {
                if v > 600 {
                    return Err(AdminServiceError::InvalidRequest(
                        "全凭据冷却 bail 阈值过大（>600s）".into(),
                    ));
                }
                config.all_credentials_cooldown_bail_threshold_secs = v;
            }

            if let Some(v) = req.balance_auto_refresh_secs {
                if v != 0 && (v < 60 || v > 86_400) {
                    return Err(AdminServiceError::InvalidRequest(
                        "余额自动刷新周期应为 0（禁用）或 60~86400 秒".into(),
                    ));
                }
                config.balance_auto_refresh_secs = v;
            }

            if let Some(v) = req.error_log_enabled {
                config.error_log_enabled = v;
            }
            if let Some(v) = req.error_log_max_count {
                if v > 10_000_000 {
                    return Err(AdminServiceError::InvalidRequest(
                        "错误日志条数上限过大（>10M）".into(),
                    ));
                }
                config.error_log_max_count = v;
            }
            if let Some(v) = req.error_log_max_age_days {
                if v > 3650 {
                    return Err(AdminServiceError::InvalidRequest(
                        "错误日志保留天数过大（>3650）".into(),
                    ));
                }
                config.error_log_max_age_days = v;
            }
            if let Some(codes) = &req.error_log_excluded_status_codes {
                let mut cleaned: Vec<u16> = codes.iter().copied().filter(|&c| c > 0).collect();
                cleaned.sort_unstable();
                cleaned.dedup();
                config.error_log_excluded_status_codes = cleaned;
            }

            config
                .save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        // 2. 持久化成功后再应用运行时变更
        let config = self.config.read();

        // 热更新 region
        if req.region.is_some() {
            self.token_manager.update_region(config.region.clone());
        }

        // 热更新 credential_rpm
        if req.credential_rpm.is_some() {
            self.token_manager
                .update_credential_rpm(config.credential_rpm);
        }

        // 热更新重试配置（任一字段被请求时同步整组到 token_manager 的运行时配置）
        if req.max_retries_per_credential.is_some()
            || req.max_total_retries.is_some()
            || req.all_credentials_cooldown_bail_threshold_secs.is_some()
        {
            self.token_manager.update_retry_config(
                Some(config.max_retries_per_credential),
                Some(config.max_total_retries),
                Some(config.all_credentials_cooldown_bail_threshold_secs),
            );
        }

        // 热更新 default_endpoint
        if req.default_endpoint.is_some() {
            self.token_manager
                .update_default_endpoint(config.default_endpoint.clone());
            if let Some(provider) = &self.kiro_provider
                && let Err(e) = provider.update_default_endpoint(config.default_endpoint.clone())
            {
                tracing::warn!("热更新 KiroProvider default_endpoint 失败: {}", e);
            }
        }

        // 热更新 Prompt Cache 运行时配置
        if req.prompt_cache_ttl_seconds.is_some() || req.prompt_cache_accounting_enabled.is_some() {
            self.prompt_cache_runtime.write().update(
                req.prompt_cache_ttl_seconds,
                req.prompt_cache_accounting_enabled,
            );
        }

        // 热更新压缩配置到运行时 Arc<RwLock<CompressionConfig>>
        if let Some(c) = &req.compression {
            let mut runtime = self.compression_config.write();
            Self::apply_compression_fields(&mut runtime, c);
        }

        Ok(())
    }

    // =====================================================================
    // 代理池 API
    // =====================================================================

    /// 列出代理池所有条目
    pub fn list_proxies(&self) -> ProxyListResponse {
        let now = chrono::Utc::now();
        let warning_hours = self.config.read().proxy_expiry_warning_hours;
        let pool = match &self.proxy_pool {
            Some(p) => p,
            None => {
                return ProxyListResponse {
                    total: 0,
                    proxies: vec![],
                    enabled: false,
                };
            }
        };
        let entries = pool.snapshot();
        let proxies: Vec<ProxyEntryItem> = entries
            .iter()
            .map(|e| {
                let remaining = (e.expires_at - now).num_seconds();
                let status = if e.is_expired(now) {
                    "expired"
                } else if !e.has_free_slot() {
                    "full"
                } else if (e.expires_at - now).num_hours() < warning_hours {
                    "expiring"
                } else {
                    "active"
                }
                .to_string();
                ProxyEntryItem {
                    id: e.id.clone(),
                    url: e.url.clone(),
                    username: e.username.clone(),
                    expires_at: e.expires_at,
                    remaining_secs: remaining.max(0),
                    slots: e.slots,
                    used_slots: e.used_slots(),
                    bound_credential_ids: e.bound_credential_ids.clone(),
                    status,
                    label: e.label.clone(),
                    created_at: e.created_at,
                    last_rotated_at: e.last_rotated_at,
                }
            })
            .collect();
        ProxyListResponse {
            total: proxies.len(),
            proxies,
            enabled: true,
        }
    }

    /// 批量导入代理（host:port:user:pass 行格式）
    pub fn import_proxies(
        &self,
        req: ImportProxiesRequest,
    ) -> Result<ImportProxiesResponse, AdminServiceError> {
        let pool = self.require_proxy_pool()?;

        // 校验协议
        let scheme = match req.scheme.to_ascii_lowercase().as_str() {
            "http" | "https" | "socks5" | "socks5h" => req.scheme.to_ascii_lowercase(),
            other => {
                return Err(AdminServiceError::InvalidRequest(format!(
                    "不支持的代理协议: {}",
                    other
                )));
            }
        };
        let slots = req.slots_per_proxy.max(1);
        let now = chrono::Utc::now();

        let mut entries: Vec<ProxyEntry> = Vec::with_capacity(req.lines.len());
        let mut items: Vec<ImportProxyItemResult> = Vec::with_capacity(req.lines.len());
        let mut placeholder_ids: Vec<usize> = Vec::new();

        for (idx, raw_line) in req.lines.iter().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                items.push(ImportProxyItemResult {
                    index: idx,
                    line: raw_line.clone(),
                    success: false,
                    proxy_id: None,
                    error: Some("空行".to_string()),
                });
                continue;
            }
            match parse_host_port_user_pass_line(line) {
                Ok((host_port, user, pass)) => {
                    let url = build_proxy_url(&scheme, &host_port);
                    let username = if user.is_empty() { None } else { Some(user) };
                    let password = if pass.is_empty() { None } else { Some(pass) };
                    placeholder_ids.push(idx);
                    entries.push(ProxyEntry {
                        id: String::new(), // 由 add_many 自动生成
                        url,
                        username,
                        password,
                        expires_at: req.default_expires_at,
                        slots,
                        bound_credential_ids: vec![],
                        label: req.label.clone(),
                        created_at: now,
                        last_rotated_at: None,
                    });
                    // 占位，等 add_many 完成后填充结果
                    items.push(ImportProxyItemResult {
                        index: idx,
                        line: raw_line.clone(),
                        success: false,
                        proxy_id: None,
                        error: None,
                    });
                }
                Err(e) => items.push(ImportProxyItemResult {
                    index: idx,
                    line: raw_line.clone(),
                    success: false,
                    proxy_id: None,
                    error: Some(e.to_string()),
                }),
            }
        }

        let results = pool.add_many(entries);
        // results 顺序对应 placeholder_ids
        let mut added = 0usize;
        let mut failed = 0usize;
        for (out_idx, res) in placeholder_ids.iter().zip(results.into_iter()) {
            let item = &mut items[*out_idx];
            match res {
                Ok(id) => {
                    item.success = true;
                    item.proxy_id = Some(id);
                    added += 1;
                }
                Err(msg) => {
                    item.error = Some(msg);
                    failed += 1;
                }
            }
        }
        // 含因解析失败导致已经标了 error 的，也并入 failed 总计
        let total_failed = items.iter().filter(|it| !it.success).count();

        Ok(ImportProxiesResponse {
            total: items.len(),
            added,
            failed: total_failed.max(failed),
            items,
        })
    }

    /// 删除单个代理
    pub fn delete_proxy(&self, id: &str, force: bool) -> Result<Vec<u64>, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let freed = pool
            .delete(id, force)
            .map_err(|e| AdminServiceError::InvalidRequest(e.to_string()))?;
        // 把被解绑的凭据清掉 proxy_slot_id 并禁用（防止回退本地）
        for cred_id in &freed {
            let _ = self.token_manager.set_proxy_slot(*cred_id, None);
            let _ = self.token_manager.set_disabled(*cred_id, true);
            self.token_manager.report_proxy_exhausted(*cred_id);
        }
        Ok(freed)
    }

    /// 批量删除代理
    pub fn batch_delete_proxies(
        &self,
        req: BatchProxyDeleteRequest,
    ) -> Result<BatchProxyResponse, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let mut items = Vec::with_capacity(req.ids.len());
        let mut success_count = 0;
        let mut fail_count = 0;
        for id in &req.ids {
            match pool.delete(id, req.force) {
                Ok(freed) => {
                    for cred_id in &freed {
                        let _ = self.token_manager.set_proxy_slot(*cred_id, None);
                        let _ = self.token_manager.set_disabled(*cred_id, true);
                        self.token_manager.report_proxy_exhausted(*cred_id);
                    }
                    success_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: true,
                        error: None,
                    });
                }
                Err(e) => {
                    fail_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(BatchProxyResponse {
            total: req.ids.len(),
            success_count,
            fail_count,
            items,
        })
    }

    /// 批量解绑（清空 boundCredentialIds，并把对应凭据 proxy_slot 置空 + 禁用）
    pub fn batch_unbind_proxies(
        &self,
        req: BatchProxyUnbindRequest,
    ) -> Result<BatchProxyResponse, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let mut items = Vec::with_capacity(req.ids.len());
        let mut success_count = 0;
        let mut fail_count = 0;
        for id in &req.ids {
            let entry = match pool.get(id) {
                Some(e) => e,
                None => {
                    fail_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: false,
                        error: Some("不存在".to_string()),
                    });
                    continue;
                }
            };
            let mut all_ok = true;
            for cred_id in &entry.bound_credential_ids {
                if let Err(e) = pool.unbind(id, *cred_id) {
                    all_ok = false;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    });
                    break;
                }
                let _ = self.token_manager.set_proxy_slot(*cred_id, None);
                let _ = self.token_manager.set_disabled(*cred_id, true);
                self.token_manager.report_proxy_exhausted(*cred_id);
            }
            if all_ok {
                success_count += 1;
                items.push(BatchProxyItemResult {
                    id: id.clone(),
                    success: true,
                    error: None,
                });
            } else {
                fail_count += 1;
            }
        }
        Ok(BatchProxyResponse {
            total: req.ids.len(),
            success_count,
            fail_count,
            items,
        })
    }

    /// 批量调整槽位
    pub fn batch_set_proxy_slots(
        &self,
        req: BatchProxySlotsRequest,
    ) -> Result<BatchProxyResponse, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let mut items = Vec::with_capacity(req.ids.len());
        let mut success_count = 0;
        let mut fail_count = 0;
        for id in &req.ids {
            match pool.set_slots(id, req.slots, req.force) {
                Ok(_) => {
                    success_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: true,
                        error: None,
                    });
                }
                Err(e) => {
                    fail_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(BatchProxyResponse {
            total: req.ids.len(),
            success_count,
            fail_count,
            items,
        })
    }

    /// 批量延长到期时间
    pub fn batch_extend_proxies(
        &self,
        req: BatchProxyExtendRequest,
    ) -> Result<BatchProxyResponse, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let mut items = Vec::with_capacity(req.ids.len());
        let mut success_count = 0;
        let mut fail_count = 0;
        for id in &req.ids {
            match pool.set_expires_at(id, req.expires_at) {
                Ok(_) => {
                    success_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: true,
                        error: None,
                    });
                }
                Err(e) => {
                    fail_count += 1;
                    items.push(BatchProxyItemResult {
                        id: id.clone(),
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(BatchProxyResponse {
            total: req.ids.len(),
            success_count,
            fail_count,
            items,
        })
    }

    /// 凭据 → 代理：手动绑定
    pub fn bind_proxy_to_credential(
        &self,
        credential_id: u64,
        req: BindProxyRequest,
    ) -> Result<(), AdminServiceError> {
        let pool = self.require_proxy_pool()?;

        // 校验凭据存在
        let snapshot = self.token_manager.snapshot();
        if !snapshot.entries.iter().any(|e| e.id == credential_id) {
            return Err(AdminServiceError::NotFound { id: credential_id });
        }

        // 若已绑定到其它代理，先解绑
        if let Some(old_slot) = snapshot
            .entries
            .iter()
            .find(|e| e.id == credential_id)
            .and_then(|e| e.proxy_slot_id.clone())
            && old_slot != req.proxy_id
        {
            let _ = pool.unbind(&old_slot, credential_id);
        }

        pool.manual_bind(&req.proxy_id, credential_id)
            .map_err(|e| AdminServiceError::InvalidRequest(e.to_string()))?;
        self.token_manager
            .set_proxy_slot(credential_id, Some(req.proxy_id.clone()))
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        if req.auto_enable {
            let _ = self.token_manager.set_disabled(credential_id, false);
        }
        Ok(())
    }

    /// 凭据 → 解绑代理（不允许"未绑定 + 启用"，所以解绑会同时禁用）
    pub fn unbind_proxy_from_credential(&self, credential_id: u64) -> Result<(), AdminServiceError> {
        let pool = self.require_proxy_pool()?;

        let snapshot = self.token_manager.snapshot();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.id == credential_id)
            .ok_or(AdminServiceError::NotFound { id: credential_id })?;
        if let Some(slot) = entry.proxy_slot_id.clone() {
            let _ = pool.unbind(&slot, credential_id);
        }
        let _ = self.token_manager.set_proxy_slot(credential_id, None);
        let _ = self.token_manager.set_disabled(credential_id, true);
        Ok(())
    }

    /// 测试单个代理（出口 IP + 延迟）
    pub async fn test_proxy(&self, id: &str) -> Result<ProxyTestResult, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let entry = pool
            .get(id)
            .ok_or_else(|| AdminServiceError::InvalidRequest(format!("代理 {} 不存在", id)))?;
        let tls = self.config.read().tls_backend;
        Ok(test_proxy(&entry, tls).await)
    }

    /// 批量测试代理（并发，最多 8 个）
    pub async fn batch_test_proxies(
        &self,
        ids: &[String],
    ) -> Result<Vec<ProxyTestResult>, AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let tls = self.config.read().tls_backend;
        let entries: Vec<ProxyEntry> = ids.iter().filter_map(|id| pool.get(id)).collect();
        if entries.is_empty() {
            return Ok(vec![]);
        }
        use futures::stream::{self, StreamExt};
        const MAX_CONCURRENT: usize = 8;
        let results = stream::iter(entries.into_iter())
            .map(|e| async move { test_proxy(&e, tls).await })
            .buffer_unordered(MAX_CONCURRENT)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    /// 立即触发一次轮换扫描
    pub async fn rotate_proxies_now(&self) -> Result<(), AdminServiceError> {
        let pool = self.require_proxy_pool()?;
        let warning_hours = self.config.read().proxy_expiry_warning_hours;
        proxy_rotation::run_one_round(&pool, &self.token_manager, warning_hours).await;
        Ok(())
    }

    /// 读取告警环形缓冲区
    pub fn list_proxy_alerts(&self) -> ProxyAlertsResponse {
        let pool = match &self.proxy_pool {
            Some(p) => p,
            None => {
                return ProxyAlertsResponse {
                    total: 0,
                    alerts: vec![],
                };
            }
        };
        let alerts = pool.alerts();
        ProxyAlertsResponse {
            total: alerts.len(),
            alerts: alerts
                .into_iter()
                .map(|a| ProxyAlertItem {
                    at: a.at,
                    level: format!("{:?}", a.level).to_lowercase(),
                    message: a.message,
                })
                .collect(),
        }
    }

    // =====================================================================

    /// 将更新请求中的压缩字段应用到目标 CompressionConfig
    fn apply_compression_fields(
        target: &mut CompressionConfig,
        src: &super::types::UpdateCompressionConfigRequest,
    ) {
        if let Some(v) = src.enabled {
            target.enabled = v;
        }
        if let Some(v) = src.whitespace_compression {
            target.whitespace_compression = v;
        }
        if let Some(ref v) = src.thinking_strategy {
            target.thinking_strategy = v.clone();
        }
        if let Some(v) = src.tool_result_max_chars {
            target.tool_result_max_chars = v;
        }
        if let Some(v) = src.tool_result_head_lines {
            target.tool_result_head_lines = v;
        }
        if let Some(v) = src.tool_result_tail_lines {
            target.tool_result_tail_lines = v;
        }
        if let Some(v) = src.tool_use_input_max_chars {
            target.tool_use_input_max_chars = v;
        }
        if let Some(v) = src.tool_description_max_chars {
            target.tool_description_max_chars = v;
        }
        if let Some(v) = src.max_history_turns {
            target.max_history_turns = v;
        }
        if let Some(v) = src.max_history_chars {
            target.max_history_chars = v;
        }
        if let Some(v) = src.max_request_body_bytes {
            target.max_request_body_bytes = v;
        }
    }
}

/// 从代理 URL 中分离出认证信息：`scheme://[user:pass@]host:port` →
/// `(scheme://host:port, user, pass)`。
fn parse_proxy_url_with_auth(
    raw: &str,
) -> anyhow::Result<(String, Option<String>, Option<String>)> {
    let raw = raw.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("URL 缺少 scheme（如 socks5://...）"))?;
    let scheme_lc = scheme.trim().to_ascii_lowercase();
    if !matches!(scheme_lc.as_str(), "http" | "https" | "socks5" | "socks5h") {
        anyhow::bail!("不支持的代理协议: {}", scheme);
    }
    // host_port 不带 path/query；只取 "@" 之后到下一个 "/" / "?" 之前
    let (auth_part, host_part) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let host_port = host_part
        .split(['/', '?'])
        .next()
        .unwrap_or(host_part)
        .trim();
    if host_port.is_empty() || !host_port.contains(':') {
        anyhow::bail!("URL 缺少 host:port");
    }
    let (username, password) = match auth_part {
        Some(a) if !a.is_empty() => match a.split_once(':') {
            Some((u, p)) => (
                Some(decode_userinfo(u)),
                if p.is_empty() {
                    None
                } else {
                    Some(decode_userinfo(p))
                },
            ),
            None => (Some(decode_userinfo(a)), None),
        },
        _ => (None, None),
    };
    Ok((format!("{}://{}", scheme_lc, host_port), username, password))
}

/// 解码 URL userinfo 段中的 percent-encoding（仅处理常见的 `%XX`，失败时返回原串）
fn decode_userinfo(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// 弹性解析代理到期时间：支持 RFC3339、`YYYY-MM-DD`、Unix 秒、Unix 毫秒。
/// 解析失败或缺省 → 默认 30 天后。
fn parse_proxy_expires_at(value: Option<&serde_json::Value>) -> chrono::DateTime<Utc> {
    use chrono::{Duration, NaiveDate, TimeZone};
    let fallback = Utc::now() + Duration::days(30);
    let v = match value {
        Some(v) => v,
        None => return fallback,
    };
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.with_timezone(&Utc);
        }
        if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d")
            && let Some(ndt) = d.and_hms_opt(0, 0, 0)
        {
            return Utc.from_utc_datetime(&ndt);
        }
        if let Ok(n) = s.parse::<i64>() {
            return ts_to_dt(n).unwrap_or(fallback);
        }
    } else if let Some(n) = v.as_i64() {
        return ts_to_dt(n).unwrap_or(fallback);
    } else if let Some(f) = v.as_f64() {
        return ts_to_dt(f as i64).unwrap_or(fallback);
    }
    fallback
}

fn ts_to_dt(n: i64) -> Option<chrono::DateTime<Utc>> {
    use chrono::TimeZone;
    // 13 位 ≈ 毫秒；10 位 ≈ 秒
    if n.abs() >= 1_000_000_000_000 {
        Utc.timestamp_millis_opt(n).single()
    } else {
        Utc.timestamp_opt(n, 0).single()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::PromptCacheRuntime;
    use crate::kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::provider::KiroProvider;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::{CompressionConfig, Config};
    use std::collections::HashSet;
    use std::env;
    use std::fs;

    fn create_test_service() -> AdminService {
        let config_path = env::temp_dir().join(format!(
            "kiro-admin-service-test-{}-{}.json",
            std::process::id(),
            fastrand::u64(..)
        ));

        let config = Arc::new(RwLock::new(Config::load(&config_path).unwrap()));
        let compression_config = Arc::new(RwLock::new(CompressionConfig::default()));
        let prompt_cache_runtime = Arc::new(RwLock::new(PromptCacheRuntime::new(300, true)));

        let credentials = KiroCredentials::default();
        let tm = Arc::new(
            MultiTokenManager::new(config.read().clone(), vec![credentials], None, None, false)
                .unwrap(),
        );

        let known_endpoints: HashSet<String> = vec!["ide".to_string(), "cli".to_string()]
            .into_iter()
            .collect();

        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint::new()));
        endpoints.insert("cli".to_string(), Arc::new(CliEndpoint::new()));
        let provider = Arc::new(KiroProvider::with_proxy(
            Arc::clone(&tm),
            None,
            endpoints,
            "ide".to_string(),
        ));

        AdminService::new(
            tm,
            Some(provider),
            config,
            compression_config,
            prompt_cache_runtime,
            known_endpoints,
        )
    }

    fn read_persisted_config(service: &AdminService) -> Config {
        let config_path = service.config.read().config_path().unwrap().to_path_buf();
        let content = fs::read_to_string(config_path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_valid() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("cli".to_string()),
            compression: None,
            auto_disable_patterns: None,
            error_replace_rules: None,
            auto_disable_usage_threshold_pct: None,
            max_retries_per_credential: None,
            max_total_retries: None,
            all_credentials_cooldown_bail_threshold_secs: None,
            balance_auto_refresh_secs: None,
            error_log_enabled: None,
            error_log_max_count: None,
            error_log_max_age_days: None,
            error_log_excluded_status_codes: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "cli");
        assert_eq!(service.token_manager.config().default_endpoint, "cli");

        let persisted = read_persisted_config(&service);
        assert_eq!(persisted.default_endpoint, "cli");
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_empty_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("".to_string()),
            compression: None,
            auto_disable_patterns: None,
            error_replace_rules: None,
            auto_disable_usage_threshold_pct: None,
            max_retries_per_credential: None,
            max_total_retries: None,
            all_credentials_cooldown_bail_threshold_secs: None,
            balance_auto_refresh_secs: None,
            error_log_enabled: None,
            error_log_max_count: None,
            error_log_max_age_days: None,
            error_log_excluded_status_codes: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("默认 endpoint 不能为空")
        );
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_whitespace_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("   ".to_string()),
            compression: None,
            auto_disable_patterns: None,
            error_replace_rules: None,
            auto_disable_usage_threshold_pct: None,
            max_retries_per_credential: None,
            max_total_retries: None,
            all_credentials_cooldown_bail_threshold_secs: None,
            balance_auto_refresh_secs: None,
            error_log_enabled: None,
            error_log_max_count: None,
            error_log_max_age_days: None,
            error_log_excluded_status_codes: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("默认 endpoint 不能为空")
        );
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_unknown_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("unknown".to_string()),
            compression: None,
            auto_disable_patterns: None,
            error_replace_rules: None,
            auto_disable_usage_threshold_pct: None,
            max_retries_per_credential: None,
            max_total_retries: None,
            all_credentials_cooldown_bail_threshold_secs: None,
            balance_auto_refresh_secs: None,
            error_log_enabled: None,
            error_log_max_count: None,
            error_log_max_age_days: None,
            error_log_excluded_status_codes: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("未知的 endpoint"));
        assert!(err_msg.contains("unknown"));
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_trimmed() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("  cli  ".to_string()),
            compression: None,
            auto_disable_patterns: None,
            error_replace_rules: None,
            auto_disable_usage_threshold_pct: None,
            max_retries_per_credential: None,
            max_total_retries: None,
            all_credentials_cooldown_bail_threshold_secs: None,
            balance_auto_refresh_secs: None,
            error_log_enabled: None,
            error_log_max_count: None,
            error_log_max_age_days: None,
            error_log_excluded_status_codes: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "cli");
        assert_eq!(service.token_manager.config().default_endpoint, "cli");

        let persisted = read_persisted_config(&service);
        assert_eq!(persisted.default_endpoint, "cli");
    }

    #[test]
    fn test_get_global_config_includes_default_endpoint() {
        let service = create_test_service();
        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "ide"); // Config::default() 的默认值
    }
}
