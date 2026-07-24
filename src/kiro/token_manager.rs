//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持单凭据 (TokenManager) 和多凭据 (MultiTokenManager) 管理
//!
//! ## 增强特性
//!
//! - **多维度设备指纹**: 每个凭据生成独立的设备指纹，模拟真实客户端
//! - **后台 Token 刷新**: 定期检查并预刷新即将过期的 Token
//! - **精细化速率限制**: 每日请求限制、请求间隔控制、指数退避
//! - **冷却管理**: 分类管理不同原因的冷却状态
//! - **优雅降级**: Token 刷新失败时使用现有 Token

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::Mutex as TokioMutex;

use crate::common::utf8::floor_char_boundary;
use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::affinity::UserAffinityManager;
use crate::kiro::background_refresh::{
    BackgroundRefreshConfig, BackgroundRefresher, RefreshResult,
};
use crate::kiro::client_profile;
use crate::kiro::cooldown::{CooldownManager, CooldownReason};
use crate::kiro::endpoint::{
    CLI_ENDPOINT_NAME, CliEndpoint, IDE_ENDPOINT_NAME, IdeEndpoint, KiroEndpoint, RequestContext,
};
use crate::kiro::machine_id;
use crate::kiro::metrics::RpmTracker;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    IdcRefreshRequest, IdcRefreshResponse, RefreshRequest, RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::kiro::rate_limiter::{RateLimitConfig, RateLimiter};
use crate::model::config::Config;

/// 对 user_id 进行掩码处理，保护隐私
fn mask_user_id(user_id: Option<&str>) -> String {
    match user_id {
        Some(id) => {
            let len = id.len();
            if len > 12 {
                // user_id 客户端可控，按字节裸切多字节 UTF-8 会 panic，回退到最近字符边界
                let prefix_end = floor_char_boundary(id, 4);
                let suffix_start = floor_char_boundary(id, len - 4);
                format!("{}***{}", &id[..prefix_end], &id[suffix_start..])
            } else {
                "***".to_string()
            }
        }
        None => "None".to_string(),
    }
}

/// Token 管理器
///
/// 负责管理凭据和 Token 的自动刷新
#[allow(dead_code)]
pub struct TokenManager {
    config: Config,
    credentials: KiroCredentials,
    proxy: Option<ProxyConfig>,
}

#[allow(dead_code)]
impl TokenManager {
    /// 创建新的 TokenManager 实例
    pub fn new(config: Config, credentials: KiroCredentials, proxy: Option<ProxyConfig>) -> Self {
        Self {
            config,
            credentials,
            proxy,
        }
    }

    /// 获取凭据的引用
    pub fn credentials(&self) -> &KiroCredentials {
        &self.credentials
    }

    /// 获取配置的引用
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 确保获取有效的访问 Token
    ///
    /// 如果 Token 过期或即将过期，会自动刷新
    pub async fn ensure_valid_token(&mut self) -> anyhow::Result<String> {
        let token_missing_or_truncated = self
            .credentials
            .access_token
            .as_deref()
            .is_none_or(|t| t.trim().is_empty() || t.ends_with("...") || t.contains("..."));

        if token_missing_or_truncated
            || is_token_expired(&self.credentials)
            || is_token_expiring_soon(&self.credentials)
        {
            self.credentials =
                refresh_token(&self.credentials, &self.config, self.proxy.as_ref()).await?;

            // 刷新后再次检查 token 时间有效性
            if is_token_expired(&self.credentials) {
                anyhow::bail!("刷新后的 Token 仍然无效或已过期");
            }
        }

        self.credentials
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))
    }

    /// 获取使用额度信息
    ///
    /// 调用 getUsageLimits API 查询当前账户的使用额度
    pub async fn get_usage_limits(&mut self) -> anyhow::Result<UsageLimitsResponse> {
        let token = self.ensure_valid_token().await?;
        get_usage_limits(&self.credentials, &self.config, &token, self.proxy.as_ref()).await
    }
}

/// 检查 Token 是否在指定时间内过期
pub(crate) fn is_token_expiring_within(
    credentials: &KiroCredentials,
    minutes: i64,
) -> Option<bool> {
    credentials
        .expires_at
        .as_ref()
        .and_then(|expires_at| DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires| expires <= Utc::now() + Duration::minutes(minutes))
}

/// 检查 Token 是否已过期（提前 5 分钟判断）
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 检查 Token 是否即将过期（10分钟内）
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

fn credential_secret_hash(credentials: &KiroCredentials) -> Option<String> {
    credentials
        .kiro_api_key
        .as_deref()
        .map(sha256_hex)
        .or_else(|| credentials.refresh_token.as_deref().map(sha256_hex))
}

fn is_invalid_grant_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("invalid_grant")
}

/// 在错误响应 JSON 里替换"错误原因"字段的值，保留外层结构。
///
/// 覆盖位置（命中任一即算成功）：
/// - 顶层 `message` / `Message` / `error_description` / `errorMessage` / `detail` / `msg` / `reason`
/// - 嵌套 `error.message` / `error.Message` / `error.errorMessage`（Anthropic / 通用 SDK 风格）
///
/// 返回 `None` 表示：不是 JSON 或没有任何已知字段命中（调用方应回退到整段替换）。
fn replace_error_message_field(body: &str, new_message: &str) -> Option<String> {
    const TOP_KEYS: &[&str] = &[
        "message",
        "Message",
        "error_description",
        "errorMessage",
        "detail",
        "msg",
        "reason",
    ];
    const NESTED_KEYS: &[&str] = &["message", "Message", "errorMessage"];

    let mut v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut replaced = false;

    if let serde_json::Value::Object(map) = &mut v {
        for key in TOP_KEYS {
            if let Some(slot) = map.get_mut(*key)
                && slot.is_string()
            {
                *slot = serde_json::Value::String(new_message.to_string());
                replaced = true;
            }
        }
        if let Some(serde_json::Value::Object(err_map)) = map.get_mut("error") {
            for key in NESTED_KEYS {
                if let Some(slot) = err_map.get_mut(*key)
                    && slot.is_string()
                {
                    *slot = serde_json::Value::String(new_message.to_string());
                    replaced = true;
                }
            }
        }
    }

    if replaced {
        serde_json::to_string(&v).ok()
    } else {
        None
    }
}

/// 内置「账号封禁/暂停」关键字（零配置生效）：命中即认定该账号被上游风控封禁，
/// 应永久隔离该凭据并切号，而不是当作普通失败反复重试。
const BUILTIN_SUSPEND_PATTERNS: &[&str] = &[
    "TEMPORARILY_SUSPENDED",
    "ACCOUNT_SUSPENDED",
    "AccountSuspendedException",
];

fn is_temporarily_suspended_error(err: &anyhow::Error) -> bool {
    let s = err.to_string();
    BUILTIN_SUSPEND_PATTERNS.iter().any(|p| s.contains(p))
}

/// 只在响应体的**结构化错误字段**（`__type`/`reason`/`message`/`error.*`）里匹配封号关键字，
/// 命中返回具体关键字。相比对整个 body 做 contains，可避免把被上游回显的**用户 prompt** 里
/// 出现的同名字符串（如 prompt 里提到 `AccountSuspendedException`）误判为封号；也天然避开
/// 瞬态 429/5xx（其 reason/message 不含这些封号码）。非 JSON 或字段缺失时返回 None。
fn structured_suspend_pattern(body: &str) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let fields = [
        v.get("__type").and_then(|x| x.as_str()),
        v.get("reason").and_then(|x| x.as_str()),
        v.get("message").and_then(|x| x.as_str()),
        v.pointer("/error/reason").and_then(|x| x.as_str()),
        v.pointer("/error/message").and_then(|x| x.as_str()),
        v.pointer("/Error/Code").and_then(|x| x.as_str()),
    ];
    for field in fields.into_iter().flatten() {
        if let Some(p) = BUILTIN_SUSPEND_PATTERNS
            .iter()
            .find(|p| field.contains(**p))
        {
            return Some(*p);
        }
    }
    None
}

/// 验证凭据的基本有效性
pub(crate) fn validate_credential_secret(credentials: &KiroCredentials) -> anyhow::Result<()> {
    if credentials.is_api_key_credential() {
        let api_key = credentials
            .kiro_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;

        if api_key.trim().is_empty() {
            bail!("kiroApiKey 为空");
        }

        return Ok(());
    }

    validate_refresh_token(credentials)
}

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;

    if refresh_token.is_empty() {
        bail!("refreshToken 为空");
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        bail!(
            "refreshToken 已被截断（长度: {} 字符）。\n\
             这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。",
            refresh_token.len()
        );
    }

    Ok(())
}

/// 刷新 Token
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    // 使用凭据自身的 ID（如果有）
    let id = credentials.id.unwrap_or(0);
    refresh_token_with_id(credentials, config, proxy, id).await
}

/// 刷新 Token（带凭证 ID）
pub(crate) async fn refresh_token_with_id(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
    _id: u64,
) -> anyhow::Result<KiroCredentials> {
    validate_refresh_token(credentials)?;

    // 根据 auth_method 选择刷新方式
    // 如果未指定 auth_method，根据是否有 clientId/clientSecret 自动判断
    let auth_method = credentials.auth_method.as_deref().unwrap_or_else(|| {
        if credentials.client_id.is_some() && credentials.client_secret.is_some() {
            "idc"
        } else {
            "social"
        }
    });

    if auth_method.eq_ignore_ascii_case("idc")
        || auth_method.eq_ignore_ascii_case("builder-id")
        || auth_method.eq_ignore_ascii_case("iam")
    {
        refresh_idc_token(credentials, config, proxy).await
    } else {
        refresh_social_token(credentials, config, proxy).await
    }
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先使用凭据级 region，未配置或为空时回退到 config.region
    let region = credentials
        .region
        .as_ref()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or(&config.region);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let kiro_version = client_profile::resolve(&machine_id, config).kiro_version;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            format!("KiroIDE-{}-{}", kiro_version, machine_id),
        )
        .header("Accept-Encoding", "gzip, compress, deflate, br")
        .header("host", &refresh_domain)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "OAuth 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OAuth 服务暂时不可用",
            _ => "Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: RefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
        tracing::info!(expires_in = %expires_in, "Social Token 刷新成功");
    } else {
        tracing::info!("Social Token 刷新成功（无过期时间）");
    }

    Ok(new_credentials)
}

/// IdC Token 刷新所需的 x-amz-user-agent header 前缀
const IDC_AMZ_USER_AGENT_PREFIX: &str = "aws-sdk-js/3.980.0";

/// IdC 账号开启超额时若 ListAvailableProfiles 拿不到 profileArn 的兜底值
/// （来自 kiro-proxy2 的发现）。仅用于 setUserPreference 这一种必填 ARN 的场景。
pub(crate) const FALLBACK_IDC_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

fn build_idc_refresh_user_agents(seed: &str, config: &Config) -> (String, String) {
    let profile = client_profile::resolve(seed, config);
    let os_name = &profile.system_version;
    let node_version = &profile.node_version;

    let x_amz_user_agent = format!("{} KiroIDE", IDC_AMZ_USER_AGENT_PREFIX);
    let user_agent = format!(
        "{} ua/2.1 os/{} lang/js md/nodejs#{} api/sso-oidc#3.980.0 m/E KiroIDE",
        IDC_AMZ_USER_AGENT_PREFIX, os_name, node_version
    );

    (x_amz_user_agent, user_agent)
}

/// 刷新 IdC Token (AWS SSO OIDC)
async fn refresh_idc_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientId"))?;
    let client_secret = credentials
        .client_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientSecret"))?;

    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);
    let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);
    // machineId 作为 client_profile 的稳定种子；IdC 凭据必有 client_id，一定能派生出来。
    let profile_seed =
        machine_id::generate_from_credentials(credentials, config).unwrap_or_default();
    let (x_amz_user_agent, user_agent) = build_idc_refresh_user_agents(&profile_seed, config);

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", &x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "IdC 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OIDC 服务暂时不可用",
            _ => "IdC Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: IdcRefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
        tracing::info!(expires_in = %expires_in, "IdC Token 刷新成功");
    } else {
        tracing::info!("IdC Token 刷新成功（无过期时间）");
    }

    // IdC 通常不返回 profile_arn（参考 CLIProxyAPIPlus：AWS SSO OIDC 用户发送
    // profileArn 反而会导致 403），但少数 SSO 场景上游会回传一个权威值。
    // 上游若返回则同步覆盖，避免多凭据切号后还在用旧 ARN（cherry-pick 70b8593）。
    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    Ok(new_credentials)
}

fn endpoint_for_credentials(
    credentials: &KiroCredentials,
    config: &Config,
) -> anyhow::Result<Box<dyn KiroEndpoint>> {
    match credentials.effective_endpoint_name(Some(&config.default_endpoint)) {
        IDE_ENDPOINT_NAME => Ok(Box::new(IdeEndpoint::new())),
        CLI_ENDPOINT_NAME => Ok(Box::new(CliEndpoint::new())),
        name => bail!("未知 endpoint: {}", name),
    }
}

/// 获取使用额度信息
pub(crate) async fn get_usage_limits(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UsageLimitsResponse> {
    tracing::debug!(
        endpoint = %credentials.effective_endpoint_name(Some(&config.default_endpoint)),
        "正在获取使用额度信息..."
    );

    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let endpoint = endpoint_for_credentials(credentials, config)?;
    let ctx = RequestContext {
        credentials,
        token,
        machine_id: &machine_id,
        config,
    };
    let usage = endpoint.usage_request_parts(&ctx)?;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let mut request = client.get(&usage.url);
    for (name, value) in usage.headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let body_text = response.text().await?;
    let data: UsageLimitsResponse = serde_json::from_str(&body_text).map_err(|e| {
        tracing::error!(
            "getUsageLimits JSON 解析失败: {}，原始响应: {}",
            e,
            body_text
        );
        anyhow::anyhow!("JSON 解析失败: {}", e)
    })?;
    Ok(data)
}

/// 设置上游 setUserPreference（开关账号超额计费）。
///
/// `status` 仅允许 "ENABLED" / "DISABLED"（大写）。
/// 上游成功返回 HTTP 200；非 200 时返回错误，错误中包含 body 文本。
pub(crate) async fn set_user_preference(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    overage_status: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<()> {
    let normalized = overage_status.trim().to_ascii_uppercase();
    if normalized != "ENABLED" && normalized != "DISABLED" {
        bail!(
            "overage_status 必须是 ENABLED 或 DISABLED，收到: {}",
            overage_status
        );
    }

    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let endpoint = endpoint_for_credentials(credentials, config)?;
    let ctx = RequestContext {
        credentials,
        token,
        machine_id: &machine_id,
        config,
    };
    let parts = endpoint.set_user_preference_parts(&ctx)?;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = serde_json::json!({
        "profileArn": parts.profile_arn,
        "overageConfiguration": { "overageStatus": normalized },
    });
    let mut request = client.post(&parts.url).json(&body);
    for (name, value) in parts.headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        // 截断长 body 避免日志爆
        let truncated = if body_text.len() > 512 {
            let end = floor_char_boundary(&body_text, 512);
            format!("{}…", &body_text[..end])
        } else {
            body_text
        };
        bail!("setUserPreference 返回 {}: {}", status, truncated);
    }
    Ok(())
}

/// Enterprise（Q Developer）账号的 profile 未必在凭据的认证 region，需要跨区探测。
/// Kiro IDE 客户端也是探这几个区。
pub(crate) const PROFILE_PROBE_REGIONS: &[&str] = &["us-east-1", "eu-central-1"];

/// 在指定 region 调 ListAvailableProfiles，返回首个 profileArn。
pub(crate) async fn list_available_profiles_in_region(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
    region: &str,
) -> anyhow::Result<String> {
    let url = format!("https://q.{}.amazonaws.com/ListAvailableProfiles", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let profile = client_profile::resolve(&machine_id, config);
    let kiro_version = &profile.kiro_version;
    let amz_user_agent = format!("aws-sdk-js/1.0.0 KiroIDE-{}-{}", kiro_version, machine_id);
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        profile.system_version, profile.node_version, kiro_version, machine_id
    );

    // ListAvailableProfiles 是轻量调用，且首请求会跨区串行探测多个 region——用较短超时
    // （20s）避免某区不可达时把首请求拖到数十秒。
    let client = build_client(proxy, 20, config.tls_backend)?;
    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {}", token))
        .header("x-amz-user-agent", &amz_user_agent)
        .header("user-agent", &user_agent)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Connection", "close")
        .body(r#"{"maxResults":10}"#)
        .send()
        .await?;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ListAvailableProfiles 返回 {}: {}", status, body_text);
    }
    let value: serde_json::Value = serde_json::from_str(&body_text)?;
    let arn = value
        .pointer("/profiles/0/arn")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("ListAvailableProfiles 响应没有 profileArn"))?;
    Ok(arn)
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

/// 凭据禁用原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisableReason {
    /// 连续失败次数过多
    FailureLimit,
    /// Token 刷新连续失败次数过多
    RefreshFailureLimit,
    /// 认证失败（如 invalid_grant）
    AuthenticationFailed,
    /// 账户被暂停
    AccountSuspended,
    /// 余额不足
    #[allow(dead_code)]
    InsufficientBalance,
    /// 模型临时不可用（全局禁用）
    ModelUnavailable,
    /// 手动禁用
    Manual,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
    QuotaExceeded,
    /// 凭据配置无效（如 authMethod=api_key 但缺少 kiroApiKey），需修正配置后重启
    InvalidConfig,
    /// 所绑代理不可用且池中无可替换代理
    ProxyUnavailable,
}

impl DisableReason {
    /// 落到 error_logs.disable_reason 列的中文描述（便于排障时直接阅读）
    pub fn as_log_str(&self) -> &'static str {
        match self {
            DisableReason::FailureLimit => "连续失败次数过多",
            DisableReason::RefreshFailureLimit => "Token 刷新连续失败",
            DisableReason::AuthenticationFailed => "认证失败",
            DisableReason::AccountSuspended => "账户暂停",
            DisableReason::InsufficientBalance => "余额不足",
            DisableReason::ModelUnavailable => "模型暂时不可用",
            DisableReason::Manual => "手动禁用",
            DisableReason::QuotaExceeded => "额度已用尽",
            DisableReason::InvalidConfig => "凭据配置无效",
            DisableReason::ProxyUnavailable => "无可用代理",
        }
    }
}

/// 单个凭据条目的状态
#[allow(dead_code)]
struct CredentialEntry {
    /// 凭据唯一 ID
    id: u64,
    /// 凭据信息
    credentials: KiroCredentials,
    /// API 调用连续失败次数
    failure_count: u32,
    /// Token 刷新连续失败次数
    refresh_failure_count: u32,
    /// 是否已禁用
    disabled: bool,
    /// 自愈原因（用于区分手动禁用 vs 自动禁用，便于自愈逻辑判断）
    auto_heal_reason: Option<AutoHealReason>,
    /// 禁用原因（公共 API 展示用）
    disable_reason: Option<DisableReason>,
    /// API 调用成功次数
    success_count: u64,
    /// API 调用累计失败次数（持久化；不随成功清零，与连续 failure_count 区分）
    error_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    last_used_at: Option<String>,
    /// 上次被 `acquire_context` 选中的时刻（用于 LRU 选号；None=从未被选中，最优）。
    /// 仅供进程内排序，重启清零；并发安全由 `entries` 的外层 Mutex 保证。
    last_acquired_at: Option<std::time::Instant>,
    /// refreshToken 的 SHA-256 哈希缓存（避免 snapshot 重复计算）
    refresh_token_hash: Option<String>,
    /// 当前并发请求数（实时）
    in_flight: Arc<AtomicU32>,
    /// 最近 60 秒 RPM 跟踪器（实时）
    rpm: Arc<RpmTracker>,
    /// 累计 429 触发次数（运行时统计；重启清零）
    rate_limit_count: Arc<AtomicU32>,
    /// 按模型累计用量（持久化；用于「产出价值」统计）
    model_usage: HashMap<String, ModelUsage>,
    /// 最近 RECENT_OUTCOMES_CAP 次请求结果环形缓冲（持久化；前端分布条用）
    recent_outcomes: VecDeque<u8>,
}

/// 最近请求结果环形缓冲容量
const RECENT_OUTCOMES_CAP: usize = 1000;
/// 请求结果类型（recent_outcomes 元素值）
const OUTCOME_SUCCESS: u8 = 0;
const OUTCOME_ERROR: u8 = 1;
const OUTCOME_RATE_LIMIT: u8 = 2;

/// 往环形缓冲追加一次请求结果，超容量时淘汰最旧的
fn push_recent_outcome(dq: &mut VecDeque<u8>, outcome: u8) {
    if dq.len() >= RECENT_OUTCOMES_CAP {
        dq.pop_front();
    }
    dq.push_back(outcome);
}

/// recent_outcomes ↔ 持久化字符串（s=成功 e=失败 r=429）
fn encode_recent_outcomes(dq: &VecDeque<u8>) -> String {
    dq.iter()
        .map(|&o| match o {
            OUTCOME_RATE_LIMIT => 'r',
            OUTCOME_ERROR => 'e',
            _ => 's',
        })
        .collect()
}

fn decode_recent_outcomes(s: &str) -> VecDeque<u8> {
    s.chars()
        .rev()
        .take(RECENT_OUTCOMES_CAP)
        .map(|c| match c {
            'r' => OUTCOME_RATE_LIMIT,
            'e' => OUTCOME_ERROR,
            _ => OUTCOME_SUCCESS,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 单个模型的累计用量（原始计数；价值在展示时按当前定价实时换算）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    /// 累计输入（非缓存）token
    #[serde(default)]
    pub input_tokens: u64,
    /// 累计输出 token
    #[serde(default)]
    pub output_tokens: u64,
    /// 累计缓存读取 token
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 累计缓存写入 token
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// 累计积分（上游 meteringEvent.usage 之和）
    #[serde(default)]
    pub credit_usage: f64,
    /// 累计调用次数
    #[serde(default)]
    pub calls: u64,
}

/// 自愈原因（内部使用，用于判断是否可自动恢复）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoHealReason {
    /// Admin API 手动禁用（不自动恢复）
    Manual,
    /// 连续失败达到阈值后自动禁用（可自动恢复）
    TooManyFailures,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
    #[allow(dead_code)]
    QuotaExceeded,
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    /// 累计失败次数（旧缓存无此字段时默认 0）
    #[serde(default)]
    error_count: u64,
    /// 累计 429 触发次数（旧缓存无此字段时默认 0）
    #[serde(default)]
    rate_limit_count: u32,
    last_used_at: Option<String>,
    /// 按模型累计用量（旧缓存无此字段时默认空）
    #[serde(default)]
    model_usage: HashMap<String, ModelUsage>,
    /// 最近请求结果序列（s=成功 e=失败 r=429，旧 → 新；旧缓存无此字段时默认空）
    #[serde(default)]
    recent_outcomes: String,
}

// ============================================================================
// Admin API 公开结构
// ============================================================================

/// 凭据条目快照（用于 Admin API 读取）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialEntrySnapshot {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 禁用原因
    pub disable_reason: Option<DisableReason>,
    /// 连续失败次数
    pub failure_count: u32,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// Token 过期时间
    pub expires_at: Option<String>,
    /// refreshToken 的 SHA-256 哈希（用于前端重复检测）
    pub refresh_token_hash: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// 已持久化的订阅等级（页面刷新后可直接展示）
    pub subscription_title: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// API 调用累计失败次数（持久化；不随成功清零）
    pub error_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 凭据级 Region（用于 Token 刷新）
    pub region: Option<String>,
    /// 凭据级 API Region（单独覆盖 API 请求）
    pub api_region: Option<String>,
    /// 最终生效的 endpoint 名称
    pub endpoint: Option<String>,
    /// 当前绑定的代理槽 ID
    pub proxy_slot_id: Option<String>,
    /// 已知最近的 overageStatus（"ENABLED" / "DISABLED"），可能为空
    pub overage_status: Option<String>,
    /// 当前并发请求数（实时）
    pub in_flight: u32,
    /// 最近 60 秒 RPM（实时）
    pub rpm: u32,
    /// 累计 429 触发次数
    pub rate_limit_count: u32,
    /// 允许超额使用：开启后即使额度用尽也不主动禁用
    pub allow_overuse: bool,
    /// 当前冷却原因（None 表示不在冷却中）
    pub cooldown_reason: Option<String>,
    /// 当前冷却剩余时间（秒）；不在冷却时为 None
    pub cooldown_remaining_secs: Option<u64>,
    /// 凭据级 RPM 上限（None 表示沿用全局 credentialRpm）
    pub credential_rpm: Option<u32>,
    /// 按模型累计用量（原始计数；价值由上层按定价换算）
    pub model_usage: HashMap<String, ModelUsage>,
    /// 最近请求结果序列（s=成功 e=失败 r=429，旧 → 新，最多 1000 个字符）
    pub recent_outcomes: String,
}

/// 凭据管理器状态快照
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSnapshot {
    /// 凭据条目列表
    pub entries: Vec<CredentialEntrySnapshot>,
    /// 总凭据数量
    pub total: usize,
    /// 可用凭据数量
    pub available: usize,
}

/// 缓存余额信息（用于 Admin API）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalanceInfo {
    /// 凭据 ID
    pub id: u64,
    /// 缓存的剩余额度
    pub remaining: f64,
    /// 缓存时间（Unix 毫秒时间戳）
    pub cached_at: u64,
    /// 缓存存活时间（秒）
    pub ttl_secs: u64,
}

/// 余额缓存条目
struct CachedBalance {
    remaining: f64,
    cached_at: std::time::Instant,
    /// 是否已初始化（区分"未获取过余额"和"余额为零"）
    initialized: bool,
    /// 最近一段时间的使用次数（用于判断高频/低频）
    recent_usage: u32,
    /// 上次重置使用计数的时间
    usage_reset_at: std::time::Instant,
}

/// 高频渠道 TTL（10 分钟）
const BALANCE_TTL_HIGH_FREQ_SECS: u64 = 600;
/// 低频渠道 TTL（30 分钟）
const BALANCE_TTL_LOW_FREQ_SECS: u64 = 1800;
/// 低余额渠道 TTL（24 小时）
const BALANCE_TTL_LOW_BALANCE_SECS: u64 = 86400;
/// 高频判定阈值（10分钟内使用超过此次数视为高频）
const HIGH_FREQ_THRESHOLD: u32 = 20;
/// 使用计数重置周期（10 分钟）
const USAGE_COUNT_RESET_SECS: u64 = 600;
/// 低余额阈值
const LOW_BALANCE_THRESHOLD: f64 = 1.0;

/// 多凭据 Token 管理器
///
/// 支持多个凭据的管理，实现负载均衡 + 故障转移策略
/// 故障统计基于 API 调用结果，而非 Token 刷新结果
///
/// ## 增强特性
///
/// - **多维度设备指纹**: 每个凭据生成独立的设备指纹
/// - **后台 Token 刷新**: 定期预刷新即将过期的 Token
/// - **精细化速率限制**: 每日请求限制、请求间隔控制
/// - **冷却管理**: 分类管理不同原因的冷却状态
/// - **优雅降级**: Token 刷新失败时使用现有 Token
#[allow(dead_code)]
pub struct MultiTokenManager {
    config: RwLock<Config>,
    proxy: RwLock<Option<ProxyConfig>>,
    /// 代理池（启用代理池时设置；启用后所有出站从凭据所绑代理槽取代理，禁止回退本地）
    proxy_pool: RwLock<Option<Arc<crate::kiro::proxy_pool::ProxyPool>>>,
    /// SQLite 持久化（启动期注入；启用后凭据写入走 SQL 而非 JSON）
    store: RwLock<Option<Arc<crate::storage::Store>>>,
    /// 凭据条目列表
    entries: Mutex<Vec<CredentialEntry>>,
    /// Token 刷新锁（每凭据一把）：避免同一凭据被并发重复刷新，
    /// 同时允许不同凭据的 token 刷新并行（大量号刷余额时的关键提速点）。
    refresh_locks: Mutex<HashMap<u64, Arc<TokioMutex<()>>>>,
    /// 凭据文件路径（用于回写）
    credentials_path: Option<PathBuf>,
    /// 是否为多凭据格式（数组格式才回写）
    is_multiple_format: bool,
    /// MODEL_TEMPORARILY_UNAVAILABLE 错误计数
    model_unavailable_count: AtomicU32,
    /// 选择抖动计数器（用于同权重候选的轮询，避免总选第一个）
    selection_rr: AtomicU64,
    /// 全局禁用恢复时间（None 表示未被全局禁用）
    global_recovery_time: Mutex<Option<DateTime<Utc>>>,
    /// 用户亲和性管理器
    affinity: UserAffinityManager,
    /// 余额缓存（用于负载均衡和故障转移时选择最优凭据）
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    /// 速率限制器
    rate_limiter: RateLimiter,
    /// 冷却管理器
    cooldown_manager: CooldownManager,
    /// 后台刷新器
    background_refresher: Option<Arc<BackgroundRefresher>>,
    /// 最近一次统计持久化时间（用于 debounce）
    last_stats_save_at: Mutex<Option<Instant>>,
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
    /// 凭据持久化串行锁：保证「取凭据快照 → 全量写库」整段互斥。
    /// replace_all_credentials 是「清空 + 批量插入」，若两个并发 persist 各自在锁外取到
    /// 不同新旧程度的快照，后提交者会用过期快照覆盖先提交者的改动（last-writer-wins 丢改动）。
    persist_lock: Mutex<()>,
    /// 最近点数消耗账本：(时间, credit)，用于「预计可撑时长」估算。
    /// 只保留最近 CREDIT_LEDGER_MAX_WINDOW 内的记录，内存态、重启清零。
    credit_ledger: Mutex<std::collections::VecDeque<(Instant, f64)>>,
}

/// 点数消耗账本最大保留窗口（1 小时）
const CREDIT_LEDGER_MAX_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);

/// 凭据可用性诊断：被禁用的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct DisabledCredentialDiag {
    id: u64,
    disable_reason: Option<DisableReason>,
    failure_count: u32,
    priority: u32,
}

/// 凭据可用性诊断：处于冷却的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct CooldownCredentialDiag {
    id: u64,
    reason: CooldownReason,
    remaining_ms: u64,
}

/// 凭据可用性诊断：被速率限制挡住的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RateLimitedCredentialDiag {
    id: u64,
    wait_ms: u64,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;

/// MODEL_TEMPORARILY_UNAVAILABLE 触发全局禁用的阈值
const MODEL_UNAVAILABLE_THRESHOLD: u32 = 2;

/// 全局禁用恢复时间（分钟）
const GLOBAL_DISABLE_RECOVERY_MINUTES: i64 = 5;

/// 额度用尽（402/QuotaExceeded）自动复检窗口（小时）。
/// 到期后重新启用凭据，下次请求会复检额度——覆盖「瞬时 402」与「月度额度重置」，
/// 避免额度类禁用只能靠人工/重启恢复。窗口内不恢复（immediately after 禁用不会立刻复活）。
const QUOTA_AUTO_RECOVERY_HOURS: i64 = 1;

/// 统计数据持久化防抖间隔
const STATS_SAVE_DEBOUNCE: StdDuration = StdDuration::from_secs(30);

/// 当所有可用凭据都进入冷却/速率限制时，如果最短等待时间不超过该阈值，
/// 继续短睡重试（平滑瞬时抖动）；超过则立即 bail，由上层返回 429 + Retry-After，
/// 避免 HTTP handler 挂到客户端超时。
///
/// 默认 2s，可通过 `Config.all_credentials_cooldown_bail_threshold_secs` 覆盖。
const DEFAULT_ALL_CREDENTIALS_COOLDOWN_BAIL_THRESHOLD: StdDuration = StdDuration::from_secs(2);

/// API 调用上下文
///
/// 绑定特定凭据的调用上下文，确保 token、credentials 和 id 的一致性
#[derive(Clone)]
pub struct CallContext {
    /// 凭据 ID（用于 report_success/report_failure）
    pub id: u64,
    /// 凭据信息（用于构建请求头）
    pub credentials: KiroCredentials,
    /// 访问 Token
    pub token: String,
    /// 凭据当前并发计数器（请求开始 inc，结束 dec）
    pub in_flight: Arc<AtomicU32>,
    /// 凭据 RPM 跟踪器
    pub rpm: Arc<RpmTracker>,
}

/// 解析 symlink 目标路径
///
/// 优先使用 `canonicalize`（解析所有 symlink 并返回绝对路径）。
/// 如果失败（例如目标文件不存在），则尝试用 `read_link` 解析一层 symlink。
/// 如果都失败，返回原路径。
#[allow(dead_code)]
fn resolve_symlink_target(path: &PathBuf) -> PathBuf {
    // 优先尝试 canonicalize（目标文件存在时最可靠）
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }

    // canonicalize 失败（目标可能不存在），尝试 read_link 解析 symlink
    if let Ok(target) = std::fs::read_link(path) {
        // read_link 返回的可能是相对路径，需要相对于 symlink 所在目录解析
        if target.is_absolute() {
            return target;
        }
        if let Some(parent) = path.parent() {
            return parent.join(target);
        }
        return target;
    }

    // 都失败，返回原路径
    path.clone()
}

impl MultiTokenManager {
    /// 创建多凭据 Token 管理器
    ///
    /// # Arguments
    /// * `config` - 应用配置
    /// * `credentials` - 凭据列表
    /// * `proxy` - 可选的代理配置
    /// * `credentials_path` - 凭据文件路径（用于回写）
    /// * `is_multiple_format` - 是否为多凭据格式（数组格式才回写）
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
        let rate_limit_config = {
            let mut cfg = RateLimitConfig::default();
            if let Some(rpm) = config.credential_rpm.filter(|&v| v > 0) {
                // RPM -> 固定间隔（ms），例如 20 RPM => 3000ms
                let interval_ms = (60_000u64 / rpm as u64).max(1);
                cfg.min_interval_ms = interval_ms;
                cfg.max_interval_ms = interval_ms;
                // 固定间隔下抖动无意义，避免反复计算造成误差
                cfg.jitter_percent = 0.0;
            }
            cfg
        };

        // 计算当前最大 ID，为没有 ID 的凭据分配新 ID
        let max_existing_id = credentials.iter().filter_map(|c| c.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;
        let mut has_new_machine_ids = false;
        let config_ref = &config;

        let entries: Vec<CredentialEntry> = credentials
            .into_iter()
            .map(|mut cred| {
                cred.canonicalize_auth_method();
                let id = cred.id.unwrap_or_else(|| {
                    let id = next_id;
                    next_id += 1;
                    cred.id = Some(id);
                    if !cred.runtime_only {
                        has_new_ids = true;
                    }
                    id
                });
                // authMethod=api_key 但缺少有效 kiroApiKey：启动即禁用，
                // 避免请求路径反复取不到 token 空转重试（修正配置后重启恢复）
                let invalid_config =
                    cred.is_api_key_credential() && validate_credential_secret(&cred).is_err();
                if invalid_config {
                    tracing::warn!(
                        "凭据 #{} 配置了 authMethod=api_key 但缺少有效 kiroApiKey，已自动禁用",
                        id
                    );
                }
                if cred.machine_id.is_none()
                    && let Some(machine_id) =
                        machine_id::generate_from_credentials(&cred, config_ref)
                {
                    cred.machine_id = Some(machine_id);
                    if !cred.runtime_only {
                        has_new_machine_ids = true;
                    }
                }
                let refresh_token_hash = credential_secret_hash(&cred);
                CredentialEntry {
                    id,
                    credentials: cred.clone(),
                    failure_count: 0,
                    refresh_failure_count: 0,
                    disabled: cred.disabled || invalid_config, // 从配置文件读取 disabled 状态
                    auto_heal_reason: if cred.disabled || invalid_config {
                        Some(AutoHealReason::Manual)
                    } else {
                        None
                    },
                    disable_reason: if invalid_config {
                        Some(DisableReason::InvalidConfig)
                    } else if cred.disabled {
                        Some(DisableReason::Manual)
                    } else {
                        None
                    },
                    success_count: 0,
                    error_count: 0,
                    last_used_at: None,
                    last_acquired_at: None,
                    refresh_token_hash,
                    in_flight: Arc::new(AtomicU32::new(0)),
                    rpm: RpmTracker::new(),
                    rate_limit_count: Arc::new(AtomicU32::new(0)),
                    model_usage: HashMap::new(),
                    recent_outcomes: VecDeque::new(),
                }
            })
            .collect();

        // 检测重复 ID
        let mut seen_ids = std::collections::HashSet::new();
        let mut duplicate_ids = Vec::new();
        for entry in &entries {
            if !seen_ids.insert(entry.id) {
                duplicate_ids.push(entry.id);
            }
        }
        if !duplicate_ids.is_empty() {
            anyhow::bail!("检测到重复的凭据 ID: {:?}", duplicate_ids);
        }

        // 初始化余额缓存（为每个凭据创建初始条目，支持负载均衡）
        let now = std::time::Instant::now();
        let initial_cache: HashMap<u64, CachedBalance> = entries
            .iter()
            .map(|e| {
                (
                    e.id,
                    CachedBalance {
                        remaining: 0.0,
                        cached_at: now,
                        initialized: false,
                        recent_usage: 0,
                        usage_reset_at: now,
                    },
                )
            })
            .collect();

        let manager = Self {
            config: RwLock::new(config),
            proxy: RwLock::new(proxy),
            proxy_pool: RwLock::new(None),
            store: RwLock::new(None),
            entries: Mutex::new(entries),
            refresh_locks: Mutex::new(HashMap::new()),
            credentials_path,
            is_multiple_format,
            model_unavailable_count: AtomicU32::new(0),
            selection_rr: AtomicU64::new(0),
            global_recovery_time: Mutex::new(None),
            affinity: UserAffinityManager::new(),
            balance_cache: Mutex::new(initial_cache),
            rate_limiter: RateLimiter::new(rate_limit_config),
            cooldown_manager: CooldownManager::new(),
            background_refresher: None,
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            persist_lock: Mutex::new(()),
            credit_ledger: Mutex::new(std::collections::VecDeque::new()),
        };

        // 同步凭据级 RPM 覆盖到 rate_limiter（避免重启后丢失节流配置）
        {
            let entries = manager.entries.lock();
            for entry in entries.iter() {
                if let Some(rpm) = entry.credentials.rpm.filter(|&v| v > 0) {
                    let interval_ms = (60_000u64 / rpm as u64).max(1);
                    manager
                        .rate_limiter
                        .set_credential_min_interval(entry.id, Some(interval_ms));
                }
            }
        }

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到配置文件
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写回配置文件");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();

        Ok(manager)
    }

    /// 获取配置的克隆
    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    /// 热更新代理配置
    pub fn update_proxy(&self, proxy: Option<ProxyConfig>) {
        *self.proxy.write() = proxy;
    }

    /// 设置代理池（启动期一次性注入；启用后所有出站强制走凭据所绑代理槽）
    pub fn set_proxy_pool(&self, pool: Arc<crate::kiro::proxy_pool::ProxyPool>) {
        *self.proxy_pool.write() = Some(pool);
    }

    /// 设置 SQLite store（启动期注入）
    pub fn set_store(&self, store: Arc<crate::storage::Store>) {
        *self.store.write() = Some(store);
    }

    /// 检查错误响应 body 是否命中"自动禁用"配置规则；命中返回该 pattern 字符串
    pub fn match_auto_disable_pattern(&self, body: &str) -> Option<String> {
        // 内置封号关键字始终生效（零配置），命中即永久隔离。只在结构化错误字段里匹配，
        // 避免把上游回显的用户 prompt 里的同名字符串误判、也避开瞬态 429/5xx。
        if let Some(p) = structured_suspend_pattern(body) {
            return Some(p.to_string());
        }
        let cfg = self.config.read();
        for p in &cfg.auto_disable_patterns {
            let trimmed = p.trim();
            if trimmed.is_empty() {
                continue;
            }
            if body.contains(trimmed) {
                return Some(trimmed.to_string());
            }
        }
        None
    }

    /// 命中"错误内容替换"规则时返回替换后的错误体；规则形如 `pattern===replacement`
    ///
    /// 替换语义（v1.1.42 起）：
    /// - 优先解析为 JSON，把 `message` / `error.message` 等"错误原因"字段的值替换为 `replacement`，
    ///   保留 JSON 外层结构（type/code/status 等不动），客户端拿到的仍是合法 JSON
    /// - 若不是 JSON、或找不到任何已知 message 字段，回退为「整段替换」（旧行为）
    pub fn match_error_replacement(&self, body: &str) -> Option<String> {
        let cfg = self.config.read();
        for rule in &cfg.error_replace_rules {
            let line = rule.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((pat, repl)) = line.split_once("===") {
                let pat = pat.trim();
                if !pat.is_empty() && body.contains(pat) {
                    return Some(
                        replace_error_message_field(body, repl).unwrap_or_else(|| repl.to_string()),
                    );
                }
            }
        }
        None
    }

    /// 取代理池（如已设置）
    #[allow(dead_code)]
    pub fn proxy_pool(&self) -> Option<Arc<crate::kiro::proxy_pool::ProxyPool>> {
        self.proxy_pool.read().clone()
    }

    /// 计算给定凭据的有效出站代理。
    ///
    /// - 启用了代理池时：必须从凭据所绑代理槽取；缺失或代理已过期 → 返回 Err（禁止回退本地）
    /// - 未启用代理池时：使用全局代理 self.proxy
    pub fn effective_proxy_for_cred(
        &self,
        cred: &KiroCredentials,
    ) -> anyhow::Result<Option<ProxyConfig>> {
        if let Some(pool) = self.proxy_pool.read().as_ref() {
            let slot_id = cred.proxy_slot_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "代理池启用，凭据 #{:?} 未绑定代理槽（不允许回退本地直连）",
                    cred.id
                )
            })?;
            let cfg = pool.proxy_config_for(slot_id)?;
            return Ok(Some(cfg));
        }
        Ok(self.proxy.read().clone())
    }

    /// 热更新全局 Region
    pub fn update_region(&self, region: String) {
        self.config.write().region = region;
    }

    /// 热更新默认 endpoint
    pub fn update_default_endpoint(&self, default_endpoint: String) {
        self.config.write().default_endpoint = default_endpoint;
    }

    /// 热更新错误响应自动禁用规则（替换式）
    pub fn update_auto_disable_patterns(&self, patterns: Vec<String>) {
        self.config.write().auto_disable_patterns = patterns;
    }

    /// 热更新错误内容替换规则（替换式）
    pub fn update_error_replace_rules(&self, rules: Vec<String>) {
        self.config.write().error_replace_rules = rules;
    }

    /// 热更新代理连续网络失败自动切换阈值（0=关闭）
    pub fn update_proxy_failure_threshold(&self, threshold: u32) {
        self.config.write().proxy_failure_threshold = threshold;
        tracing::info!("代理连续失败切换阈值已热更新为 {}", threshold);
    }

    /// 把一次「凭据被自动禁用」事件落到 error_logs 表。
    ///
    /// 若 store 未注入或写入失败，仅打 warn 日志，不会向调用方传播错误。
    /// summary 控制在 200 字符以内；request_body / response_body 由调用方自行决定是否截断
    /// （与现有错误日志一致）。
    ///
    /// # 参数
    /// - `credential_id`: 被禁用的凭据 ID
    /// - `reason`: 禁用原因（落到 disable_reason 列，error_kind 固定为 "credential_disabled"）
    /// - `upstream_status`: 触发禁用的上游 HTTP 状态码（可空）
    /// - `summary`: 简短描述（含命中的 pattern 或异常字符串）
    /// - `request_body` / `response_body`: 触发禁用的请求与响应（可空）
    /// - `request_path` / `model` / `user_id`: 关联请求信息（可空）
    #[allow(clippy::too_many_arguments)]
    pub fn record_disable_event(
        &self,
        credential_id: u64,
        reason: DisableReason,
        upstream_status: Option<u16>,
        summary: impl Into<String>,
        request_body: Option<String>,
        response_body: Option<String>,
        request_path: Option<String>,
        model: Option<String>,
        user_id: Option<String>,
    ) {
        let Some(store) = self.store.read().clone() else {
            return;
        };
        let summary = {
            let s: String = summary.into();
            if s.chars().count() > 200 {
                let head: String = s.chars().take(200).collect();
                format!("{}…", head)
            } else {
                s
            }
        };
        let insert = crate::storage::ErrorLogInsert {
            at: chrono::Utc::now(),
            credential_id: Some(credential_id),
            endpoint: None,
            // 「凭据被禁用」事件本身没有给客户端返回的 status_code，
            // 这里复用 upstream_status 作为兜底（无上游响应时填 0）
            status_code: upstream_status.unwrap_or(0),
            upstream_status,
            error_kind: "credential_disabled".to_string(),
            model,
            summary,
            request_method: None,
            request_path,
            request_headers: None,
            response_headers: None,
            request_body,
            response_body,
            user_id,
            request_id: None,
            disable_reason: Some(reason.as_log_str().to_string()),
        };
        tokio::spawn(async move {
            if let Err(e) = store.insert_error_log(&insert) {
                tracing::warn!(error = %e, "写入凭据禁用事件日志失败");
            }
        });
    }

    /// 热更新重试相关配置（max_retries_per_credential / max_total_retries /
    /// all_credentials_cooldown_bail_threshold_secs）
    pub fn update_retry_config(
        &self,
        max_retries_per_credential: Option<u32>,
        max_total_retries: Option<u32>,
        all_credentials_cooldown_bail_threshold_secs: Option<u64>,
    ) {
        let mut cfg = self.config.write();
        if let Some(v) = max_retries_per_credential {
            cfg.max_retries_per_credential = v;
        }
        if let Some(v) = max_total_retries {
            cfg.max_total_retries = v;
        }
        if let Some(v) = all_credentials_cooldown_bail_threshold_secs {
            cfg.all_credentials_cooldown_bail_threshold_secs = v;
        }
    }

    /// 热更新 429 限流冷却相关运行时参数。
    ///
    /// token_manager 持有 Config 的独立副本（provider.rs 读 cfg 走的就是这份），
    /// 因此 admin 写入磁盘后必须显式同步过来，否则修改要重启才生效。
    pub fn update_rate_limit_runtime(
        &self,
        rate_limit_cooldown_min_secs: Option<u64>,
        rate_limit_cooldown_max_secs: Option<u64>,
        capacity_pressure_cooldown_secs: Option<u64>,
        rate_limit_ignore_retry_after: Option<bool>,
        rate_limit_follow_retry_after: Option<bool>,
        rate_limit_disable_cooldown: Option<bool>,
    ) {
        let mut cfg = self.config.write();
        if let Some(v) = rate_limit_cooldown_min_secs {
            cfg.rate_limit_cooldown_min_secs = v;
        }
        if let Some(v) = rate_limit_cooldown_max_secs {
            cfg.rate_limit_cooldown_max_secs = v;
        }
        if let Some(v) = capacity_pressure_cooldown_secs {
            cfg.capacity_pressure_cooldown_secs = v;
        }
        if let Some(v) = rate_limit_ignore_retry_after {
            cfg.rate_limit_ignore_retry_after = v;
        }
        if let Some(v) = rate_limit_follow_retry_after {
            cfg.rate_limit_follow_retry_after = v;
        }
        if let Some(v) = rate_limit_disable_cooldown {
            cfg.rate_limit_disable_cooldown = v;
        }
    }

    /// 热更新单凭据目标请求速率（RPM）
    pub fn update_credential_rpm(&self, rpm: Option<u32>) {
        // 更新 config 中的 credential_rpm
        self.config.write().credential_rpm = rpm;

        // 重新计算 RateLimitConfig 并应用到 rate_limiter
        let mut cfg = RateLimitConfig::default();
        if let Some(rpm) = rpm.filter(|&v| v > 0) {
            let interval_ms = (60_000u64 / rpm as u64).max(1);
            cfg.min_interval_ms = interval_ms;
            cfg.max_interval_ms = interval_ms;
            cfg.jitter_percent = 0.0;
        }
        self.rate_limiter.update_config(cfg);
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    /// 按给定 ID 列表取出完整凭据数据（含 refresh_token / client_secret 等敏感字段）。
    ///
    /// 仅供 Admin 导出功能使用。结果顺序与入参 `ids` 一致；不存在的 ID 会被忽略
    /// （上层据此回报"跳过"原因）。
    pub fn export_credentials_by_ids(&self, ids: &[u64]) -> Vec<KiroCredentials> {
        let entries = self.entries.lock();
        ids.iter()
            .filter_map(|id| {
                entries
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| e.credentials.clone())
            })
            .collect()
    }

    /// 输出一份"为什么当前没有可用凭据"的诊断信息（用于排障）
    ///
    /// 注意：该方法只在 DEBUG 日志级别开启时执行，避免给正常路径引入额外开销。
    fn debug_log_availability_diagnostics(
        &self,
        event: &'static str,
        tried_ids: &[u64],
        min_wait: Option<std::time::Duration>,
        min_wait_detail: Option<(u64, &'static str, std::time::Duration)>,
    ) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }

        // 先快照 entries，避免在持有 entries 锁时再去访问 rate_limiter/cooldown_manager。
        let (total, mut enabled_ids, mut disabled) = {
            let entries = self.entries.lock();
            let mut enabled_ids: Vec<u64> = Vec::with_capacity(entries.len());
            let mut disabled: Vec<DisabledCredentialDiag> = Vec::new();

            for e in entries.iter() {
                if e.disabled {
                    disabled.push(DisabledCredentialDiag {
                        id: e.id,
                        disable_reason: e.disable_reason,
                        failure_count: e.failure_count,
                        priority: e.credentials.priority,
                    });
                } else {
                    enabled_ids.push(e.id);
                }
            }

            (entries.len(), enabled_ids, disabled)
        };

        enabled_ids.sort_unstable();
        disabled.sort_by_key(|d| d.id);

        let enabled_total = enabled_ids.len();
        let disabled_total = disabled.len();

        let mut cooldowns: Vec<CooldownCredentialDiag> = Vec::new();
        let mut rate_limited: Vec<RateLimitedCredentialDiag> = Vec::new();
        let mut ready: Vec<u64> = Vec::new();

        for id in &enabled_ids {
            if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(*id) {
                cooldowns.push(CooldownCredentialDiag {
                    id: *id,
                    reason,
                    remaining_ms: remaining.as_millis() as u64,
                });
                continue;
            }

            match self.rate_limiter.check_rate_limit(*id) {
                Ok(()) => ready.push(*id),
                Err(wait) => rate_limited.push(RateLimitedCredentialDiag {
                    id: *id,
                    wait_ms: wait.as_millis() as u64,
                }),
            }
        }

        cooldowns.sort_by_key(|c| (c.remaining_ms, c.id));
        rate_limited.sort_by_key(|r| (r.wait_ms, r.id));
        ready.sort_unstable();

        // 基于诊断时刻的 check_rate_limit/check_cooldown 计算"下一次可能可用"的最短等待
        let computed_min_wait_ms = cooldowns
            .iter()
            .map(|c| c.remaining_ms)
            .chain(rate_limited.iter().map(|r| r.wait_ms))
            .min();

        let min_wait_ms = min_wait.map(|d| d.as_millis() as u64);
        let (min_wait_from_id, min_wait_source, min_wait_source_ms) = match min_wait_detail {
            Some((id, source, d)) => (Some(id), Some(source), Some(d.as_millis() as u64)),
            None => (None, None, None),
        };

        tracing::debug!(
            event = event,
            total = total,
            enabled_total = enabled_total,
            disabled_total = disabled_total,
            tried = tried_ids.len(),
            tried_ids = ?tried_ids,
            config_credential_rpm = ?self.config.read().credential_rpm,
            min_wait_ms = ?min_wait_ms,
            min_wait_from_id = ?min_wait_from_id,
            min_wait_source = ?min_wait_source,
            min_wait_source_ms = ?min_wait_source_ms,
            computed_min_wait_ms = ?computed_min_wait_ms,
            disabled = ?disabled,
            cooldowns = ?cooldowns,
            rate_limited = ?rate_limited,
            ready = ?ready,
            "凭据可用性诊断"
        );
    }

    /// 选择最佳凭据（两级排序：使用次数最少 + 余额最多；完全相同则轮询）
    /// LRU 选号：从 `candidates` 里挑出"距离上次被选中最久"的那个（None = 从未被选中，最优）。
    ///
    /// 仅依赖本地 `last_acquired_at`（在 `acquire_context` 选中那一刻更新），与远端
    /// 余额接口的 `recent_usage` 解耦——后者刷新延迟可达 10~30 分钟，不适合做实时分流。
    ///
    /// 入参 `candidates` 形如 `(id, last_acquired_at)`；调用方负责在持有 entries 锁时
    /// 收集这两个字段，以避免本方法再上锁。完全并列时用全局 `selection_rr` 兜底轮询，
    /// 防止首项独占。
    fn select_best_candidate_id(
        &self,
        candidates: &[(u64, Option<std::time::Instant>)],
    ) -> Option<u64> {
        if candidates.is_empty() {
            return None;
        }

        // 第一优先级：从未被选中（None）的优先；都用过则取 last_acquired_at 最早的那个
        let oldest = candidates
            .iter()
            .map(|(_, t)| *t)
            .min_by(|a, b| match (a, b) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => x.cmp(y),
            })?;

        let tied: Vec<u64> = candidates
            .iter()
            .filter(|(_, t)| *t == oldest)
            .map(|(id, _)| *id)
            .collect();

        if tied.len() == 1 {
            return Some(tied[0]);
        }

        // 兜底：完全并列（最常见于"全是 None / 全是从未用过"）走轮询
        let rr = self.selection_rr.fetch_add(1, Ordering::Relaxed) as usize;
        Some(tied[rr % tied.len()])
    }

    /// 获取 API 调用上下文
    ///
    /// 返回绑定了 id、credentials 和 token 的调用上下文
    /// 确保整个 API 调用过程中使用一致的凭据信息
    ///
    /// 选择策略：按优先级选择可用凭据
    /// 如果 Token 过期或即将过期，会自动刷新
    /// Token 刷新失败时会尝试下一个可用凭据（不计入失败次数）
    pub async fn acquire_context(&self) -> anyhow::Result<CallContext> {
        self.acquire_context_scoped(None).await
    }

    /// 统计作用域内的凭据总数（allowed=None 时等于全部）
    fn scoped_total(&self, allowed: Option<&HashSet<u64>>) -> usize {
        match allowed {
            None => self.total_count(),
            Some(a) => {
                let entries = self.entries.lock();
                entries.iter().filter(|e| a.contains(&e.id)).count()
            }
        }
    }

    /// 统计作用域内「未禁用」的凭据数（allowed=None 时等于 available_count）
    fn scoped_available(&self, allowed: Option<&HashSet<u64>>) -> usize {
        match allowed {
            None => self.available_count(),
            Some(a) => {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .filter(|e| !e.disabled && a.contains(&e.id))
                    .count()
            }
        }
    }

    /// 获取 API 调用上下文，可限定在 `allowed` 凭据集合内（None = 全部可用）。
    ///
    /// 用于 API Key 级「允许使用的凭据范围」：候选收集、耗尽判定、自愈后重收集
    /// 均按作用域过滤；作用域内没有可用凭据时返回错误（由上层映射为 503/429）。
    pub async fn acquire_context_scoped(
        &self,
        allowed: Option<&HashSet<u64>>,
    ) -> anyhow::Result<CallContext> {
        // 检查是否需要自动恢复
        self.check_and_recover();

        // 读取冷却快速 bail 阈值（来自 Config，0 表示禁用快速 bail）
        let cooldown_bail_threshold = {
            let secs = self
                .config
                .read()
                .all_credentials_cooldown_bail_threshold_secs;
            if secs == 0 {
                StdDuration::from_secs(u64::MAX / 2) // 用极大值代表"永不 bail"
            } else {
                StdDuration::from_secs(secs)
            }
        };
        let _ = DEFAULT_ALL_CREDENTIALS_COOLDOWN_BAIL_THRESHOLD; // 仅作默认值参考

        let total = self.scoped_total(allowed);
        let mut tried_ids: Vec<u64> = Vec::new();
        // 当所有凭据都因“临时不可用”（冷却/速率限制）被跳过时，等待最短可用时间再重试。
        let mut min_wait: Option<std::time::Duration> = None;
        // 记录最短等待时间来自哪个凭据/原因，便于排障定位（冷却 vs 速率限制）。
        let mut min_wait_detail: Option<(u64, &'static str, std::time::Duration)> = None;
        // 追踪仅因冷却/速率限制被跳过的凭据数量。
        // 只有当“所有被跳过的凭据都是冷却/限流”时才触发 429 + Retry-After；
        // 若混杂了 token 刷新失败等非临时性错误，应走常规 sleep-retry 路径保留原有语义。
        let mut cooling_skipped: usize = 0;

        loop {
            // tried_ids 只会记录“本轮已经尝试过的可用凭据”（disabled 的不会被选中）。
            // 因此当存在部分 disabled 凭据时，tried_ids.len() 可能永远达不到 total，
            // 但已用尽所有可用凭据（常见于：全部被速率限制/冷却短暂挡住）。
            //
            // 这里用 available_count() 判断“可用集合是否已被尝试完”，避免误报
            // "所有凭据均已禁用（x/y）" 这类与事实不符的错误。
            let enabled_total = self.scoped_available(allowed);
            if enabled_total > 0 && tried_ids.len() >= enabled_total {
                if let Some(wait) = min_wait {
                    // 仅当本轮所有被跳过的凭据都因冷却/限流时，才以 429 + Retry-After 快速返回；
                    // 若混杂 token 刷新失败等非临时性错误，保留原有 sleep-retry 语义以避免吞掉真实错误。
                    let all_due_to_cooling = cooling_skipped == tried_ids.len();
                    if all_due_to_cooling && wait > cooldown_bail_threshold {
                        self.debug_log_availability_diagnostics(
                            "enabled_exhausted_bail_long_wait",
                            &tried_ids,
                            min_wait,
                            min_wait_detail,
                        );
                        // Retry-After 语义要求向上取整，避免客户端在实际等待结束前提前重试。
                        let secs = (wait.as_millis().div_ceil(1000) as u64).max(1);
                        let (cid, source) = min_wait_detail
                            .map(|(id, src, _)| (id, src))
                            .unwrap_or((0, "unknown"));
                        anyhow::bail!(
                            "所有凭据均处于冷却/速率限制（retry_after_secs={}，原因：{}，来自凭据 #{}）",
                            secs,
                            source,
                            cid
                        );
                    }
                    self.debug_log_availability_diagnostics(
                        "enabled_exhausted_sleep",
                        &tried_ids,
                        min_wait,
                        min_wait_detail,
                    );
                    tokio::time::sleep(wait).await;
                    tried_ids.clear();
                    cooling_skipped = 0;
                    min_wait = None;
                    min_wait_detail = None;
                    continue;
                }
                self.debug_log_availability_diagnostics(
                    "enabled_exhausted_bail",
                    &tried_ids,
                    min_wait,
                    min_wait_detail,
                );
                anyhow::bail!(
                    "所有可用凭据均无法获取有效 Token（可用: {}/{}）",
                    enabled_total,
                    total
                );
            }

            if tried_ids.len() >= total {
                if let Some(wait) = min_wait {
                    let all_due_to_cooling = cooling_skipped == tried_ids.len();
                    if all_due_to_cooling && wait > cooldown_bail_threshold {
                        self.debug_log_availability_diagnostics(
                            "total_exhausted_bail_long_wait",
                            &tried_ids,
                            min_wait,
                            min_wait_detail,
                        );
                        let secs = (wait.as_millis().div_ceil(1000) as u64).max(1);
                        let (cid, source) = min_wait_detail
                            .map(|(id, src, _)| (id, src))
                            .unwrap_or((0, "unknown"));
                        anyhow::bail!(
                            "所有凭据均处于冷却/速率限制（retry_after_secs={}，原因：{}，来自凭据 #{}）",
                            secs,
                            source,
                            cid
                        );
                    }
                    self.debug_log_availability_diagnostics(
                        "total_exhausted_sleep",
                        &tried_ids,
                        min_wait,
                        min_wait_detail,
                    );
                    tokio::time::sleep(wait).await;
                    tried_ids.clear();
                    cooling_skipped = 0;
                    min_wait = None;
                    min_wait_detail = None;
                    continue;
                }
                self.debug_log_availability_diagnostics(
                    "total_exhausted_bail",
                    &tried_ids,
                    min_wait,
                    min_wait_detail,
                );
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.scoped_available(allowed),
                    total
                );
            }

            // 把"收集候选 → LRU 选号 → 更新 last_acquired_at → 取 credentials"全部放进
            // 同一个 entries 锁内完成，避免在锁外做 select 时拿到过期快照——否则高并发
            // 下多个请求会看到同一个"最旧的号"，瞬时把流量打偏到单号上。
            //
            // cooldown / rate_limiter 检查放在锁外（它们各自维护独立的锁，不需要 entries
            // 持有）。这两个检查未通过时凭据已被标记为"刚选过"，下一轮会被往后排，对均匀
            // 分配更有利。
            let (id, credentials) = {
                let mut entries = self.entries.lock();

                let in_scope = |id: u64| allowed.map(|a| a.contains(&id)).unwrap_or(true);
                let mut candidates: Vec<(u64, u32, bool, Option<std::time::Instant>)> = entries
                    .iter()
                    .filter(|e| !e.disabled && !tried_ids.contains(&e.id) && in_scope(e.id))
                    .map(|e| {
                        (
                            e.id,
                            e.credentials.priority,
                            e.credentials.runtime_only,
                            e.last_acquired_at,
                        )
                    })
                    .collect();

                // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                if candidates.is_empty()
                    && entries.iter().any(|e| {
                        e.disabled && e.auto_heal_reason == Some(AutoHealReason::TooManyFailures)
                    })
                {
                    tracing::warn!(
                        "所有凭据均已被自动禁用，执行自愈：重置失败计数并重新启用（等价于重启）"
                    );
                    for e in entries.iter_mut() {
                        if e.auto_heal_reason == Some(AutoHealReason::TooManyFailures) {
                            e.disabled = false;
                            e.auto_heal_reason = None;
                            e.disable_reason = None;
                            e.failure_count = 0;
                            // 自愈"等价于重启"语义：清掉 LRU 时间戳，让自愈后的凭据按
                            // "从未用过"重新加入轮转，避免靠旧时间戳被错误排在队尾或队首
                            e.last_acquired_at = None;
                        }
                    }

                    candidates = entries
                        .iter()
                        .filter(|e| !e.disabled && !tried_ids.contains(&e.id) && in_scope(e.id))
                        .map(|e| {
                            (
                                e.id,
                                e.credentials.priority,
                                e.credentials.runtime_only,
                                e.last_acquired_at,
                            )
                        })
                        .collect();
                }

                if candidates.is_empty() {
                    let available = entries
                        .iter()
                        .filter(|e| !e.disabled && in_scope(e.id))
                        .count();
                    if available == 0 {
                        // 作用域内一个可用凭据都没有（被限定的凭据全被禁用/删除，或范围为空集）
                        anyhow::bail!("所有凭据均已禁用（{}/{}）", available, total);
                    }
                    anyhow::bail!(
                        "所有可用凭据均已尝试（可用: {}/{}，已尝试: {}/{}）",
                        available,
                        total,
                        tried_ids.len(),
                        available
                    );
                }

                // 按优先级选出候选集合；同优先级时，优先选择仅运行时的环境变量凭据，再做 LRU 选择
                let min_priority = candidates.iter().map(|(_, p, _, _)| *p).min().unwrap_or(0);
                let prefer_runtime_only = candidates
                    .iter()
                    .any(|(_, p, runtime_only, _)| *p == min_priority && *runtime_only);
                let candidates_for_select: Vec<(u64, Option<std::time::Instant>)> = candidates
                    .iter()
                    .filter(|(_, p, runtime_only, _)| {
                        *p == min_priority && (!prefer_runtime_only || *runtime_only)
                    })
                    .map(|(id, _, _, last_acq)| (*id, *last_acq))
                    .collect();
                let id = self
                    .select_best_candidate_id(&candidates_for_select)
                    .ok_or_else(|| anyhow::anyhow!("没有可用凭据"))?;

                // LRU 关键点：选中那一刻就更新 last_acquired_at（仍持 entries 锁），
                // 后续并发请求立即能看到该号"刚被选过"，自然轮转到别的号。
                let entry = entries
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?;
                entry.last_acquired_at = Some(std::time::Instant::now());
                (id, entry.credentials.clone())
            };

            // 冷却/速率限制：把“临时不可用”的凭据视为本轮不可选，从而自然分流到其他凭据。
            if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(id) {
                tracing::trace!(
                    credential_id = %id,
                    reason = ?reason,
                    remaining_ms = %remaining.as_millis(),
                    "凭据处于冷却，跳过"
                );
                if min_wait.map(|w| remaining < w).unwrap_or(true) {
                    min_wait_detail = Some((id, "cooldown", remaining));
                }
                min_wait = Some(min_wait.map(|w| w.min(remaining)).unwrap_or(remaining));
                tried_ids.push(id);
                cooling_skipped += 1;
                continue;
            }
            if let Err(wait) = self.rate_limiter.try_acquire(id) {
                tracing::trace!(
                    credential_id = %id,
                    wait_ms = %wait.as_millis(),
                    "凭据触发速率限制，跳过"
                );
                if min_wait.map(|w| wait < w).unwrap_or(true) {
                    min_wait_detail = Some((id, "rate_limit", wait));
                }
                min_wait = Some(min_wait.map(|w| w.min(wait)).unwrap_or(wait));
                tried_ids.push(id);
                cooling_skipped += 1;
                continue;
            }

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials).await {
                Ok(ctx) => {
                    return Ok(ctx);
                }
                Err(e) => {
                    tracing::warn!("凭据 #{} Token 刷新失败，尝试下一个凭据: {}", id, e);
                    tried_ids.push(id);
                }
            }
        }
    }

    /// 获取指定用户的 API 调用上下文（带亲和性）
    ///
    /// 如果用户已绑定凭据且该凭据可用，优先使用绑定的凭据
    /// 否则使用默认的 acquire_context() 逻辑并建立新绑定
    pub async fn acquire_context_for_user(
        &self,
        user_id: Option<&str>,
        allowed: Option<&HashSet<u64>>,
    ) -> anyhow::Result<CallContext> {
        // 无 user_id 时走默认逻辑（仍受作用域限制）
        let user_id = match user_id {
            Some(id) if !id.is_empty() => id,
            _ => return self.acquire_context_scoped(allowed).await,
        };

        // 默认保持用户绑定（用于连续对话）。当绑定凭据“临时不可用”（速率限制/短冷却）时，
        // 允许分流到其他凭据，但不强制重绑，避免频繁抖动。
        let mut keep_affinity_binding = false;

        if let Some(bound_id) = self.affinity.get(user_id) {
            // 绑定凭据必须在作用域内，否则本次不复用（走 scoped 调度重选）
            let in_scope = allowed.map(|a| a.contains(&bound_id)).unwrap_or(true);
            let is_enabled = in_scope && {
                let entries = self.entries.lock();
                entries.iter().any(|e| e.id == bound_id && !e.disabled)
            };

            if is_enabled {
                if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(bound_id) {
                    // 对“长冷却”原因不保留绑定，避免长期命中后每次都先失败再回退。
                    keep_affinity_binding = matches!(
                        reason,
                        CooldownReason::RateLimitExceeded
                            | CooldownReason::TokenRefreshFailed
                            | CooldownReason::ServerError
                            | CooldownReason::ModelUnavailable
                    );
                    tracing::debug!(
                        user_id = %user_id,
                        credential_id = %bound_id,
                        reason = ?reason,
                        remaining_ms = %remaining.as_millis(),
                        keep_affinity_binding = %keep_affinity_binding,
                        "亲和性绑定凭据处于冷却，本次将分流"
                    );
                } else if let Err(wait) = self.rate_limiter.check_rate_limit(bound_id) {
                    // 只读检查，不消耗速率限制配额
                    keep_affinity_binding = true;
                    tracing::info!(
                        user_id = %mask_user_id(Some(user_id)),
                        credential_id = %bound_id,
                        wait_ms = %wait.as_millis(),
                        "亲和性绑定凭据触发速率限制，本次将分流"
                    );
                } else if let Err(wait) = self.rate_limiter.try_acquire(bound_id) {
                    // check_rate_limit 通过但 try_acquire 竞争失败（TOCTOU），保留绑定分流
                    keep_affinity_binding = true;
                    tracing::debug!(
                        user_id = %mask_user_id(Some(user_id)),
                        credential_id = %bound_id,
                        wait_ms = %wait.as_millis(),
                        "亲和性凭据 try_acquire 竞争失败，本次将分流"
                    );
                } else {
                    let credentials = {
                        let entries = self.entries.lock();
                        entries
                            .iter()
                            .find(|e| e.id == bound_id)
                            .map(|e| e.credentials.clone())
                    };

                    match credentials {
                        Some(creds) => match self.try_ensure_token(bound_id, &creds).await {
                            Ok(ctx) => {
                                self.affinity.touch(user_id);
                                return Ok(ctx);
                            }
                            Err(e) => {
                                tracing::debug!(
                                    user_id = %user_id,
                                    credential_id = %bound_id,
                                    error = %e,
                                    "亲和性绑定凭据 token 获取/刷新失败，本次将分流"
                                );
                            }
                        },
                        None => {
                            tracing::warn!(
                                user_id = %user_id,
                                credential_id = %bound_id,
                                "亲和性命中但凭据不存在，本次将分流"
                            );
                        }
                    }
                }
            }
        }

        let ctx = self.acquire_context_scoped(allowed).await?;
        if !keep_affinity_binding {
            self.affinity.set(user_id, ctx.id);
        }
        Ok(ctx)
    }

    /// 获取缓存的余额（用于故障转移选择）
    #[allow(dead_code)]
    fn get_cached_balance(&self, id: u64) -> f64 {
        let cache = self.balance_cache.lock();
        if let Some(entry) = cache.get(&id) {
            // 动态 TTL：低余额 > 低频 > 高频
            let ttl = if entry.remaining < LOW_BALANCE_THRESHOLD {
                BALANCE_TTL_LOW_BALANCE_SECS
            } else if entry.recent_usage >= HIGH_FREQ_THRESHOLD {
                BALANCE_TTL_HIGH_FREQ_SECS
            } else {
                BALANCE_TTL_LOW_FREQ_SECS
            };
            if entry.cached_at.elapsed().as_secs() < ttl {
                return entry.remaining;
            }
        }
        // 缓存不存在或过期，返回 0（会回退到优先级选择）
        0.0
    }

    /// 更新余额缓存
    pub fn update_balance_cache(&self, id: u64, remaining: f64) {
        let mut cache = self.balance_cache.lock();
        let now = std::time::Instant::now();
        // 保留现有使用计数
        let (recent_usage, usage_reset_at) = cache
            .get(&id)
            .map(|e| (e.recent_usage, e.usage_reset_at))
            .unwrap_or((0, now));
        cache.insert(
            id,
            CachedBalance {
                remaining,
                cached_at: now,
                initialized: true,
                recent_usage,
                usage_reset_at,
            },
        );
    }

    /// 从持久化缓存恢复余额信息（用于服务启动后恢复 Admin UI 展示）
    pub fn restore_balance_cache(&self, id: u64, remaining: f64, cached_at_unix_secs: f64) {
        let mut cache = self.balance_cache.lock();
        let now_instant = std::time::Instant::now();
        let now_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let age_secs = (now_unix_secs - cached_at_unix_secs).max(0.0);
        // 若系统 uptime < age_secs（如刚重启），checked_sub 会返回 None，
        // 此时设为足够旧的时间点（now - 24h），确保 TTL 判定视为已过期
        let restored_cached_at = now_instant
            .checked_sub(std::time::Duration::from_secs_f64(age_secs))
            .unwrap_or_else(|| {
                now_instant
                    .checked_sub(std::time::Duration::from_secs(86400))
                    .unwrap_or(now_instant)
            });

        let (recent_usage, usage_reset_at) = cache
            .get(&id)
            .map(|e| (e.recent_usage, e.usage_reset_at))
            .unwrap_or((0, now_instant));

        cache.insert(
            id,
            CachedBalance {
                remaining,
                cached_at: restored_cached_at,
                initialized: true,
                recent_usage,
                usage_reset_at,
            },
        );
    }

    /// 检查是否需要刷新余额缓存
    pub fn should_refresh_balance(&self, id: u64) -> bool {
        let cache = self.balance_cache.lock();
        if let Some(entry) = cache.get(&id) {
            // 未初始化的缓存需要立即刷新
            if !entry.initialized {
                return true;
            }
            // 使用动态 TTL 判断是否过期
            let ttl = if entry.remaining < LOW_BALANCE_THRESHOLD {
                BALANCE_TTL_LOW_BALANCE_SECS
            } else if entry.recent_usage >= HIGH_FREQ_THRESHOLD {
                BALANCE_TTL_HIGH_FREQ_SECS
            } else {
                BALANCE_TTL_LOW_FREQ_SECS
            };
            entry.cached_at.elapsed().as_secs() >= ttl
        } else {
            true // 无缓存，需要刷新
        }
    }

    /// 记录凭据使用（用于动态 TTL 计算和负载均衡）
    pub fn record_usage(&self, id: u64) {
        let mut cache = self.balance_cache.lock();
        let now = std::time::Instant::now();
        if let Some(entry) = cache.get_mut(&id) {
            // 重置周期过期则清零
            if entry.usage_reset_at.elapsed().as_secs() >= USAGE_COUNT_RESET_SECS {
                entry.recent_usage = 1;
                entry.usage_reset_at = now;
            } else {
                entry.recent_usage = entry.recent_usage.saturating_add(1);
            }
        } else {
            // 缓存条目不存在时创建新条目（余额未知设为 0）
            cache.insert(
                id,
                CachedBalance {
                    remaining: 0.0,
                    cached_at: now,
                    initialized: false,
                    recent_usage: 1,
                    usage_reset_at: now,
                },
            );
        }
    }

    /// 获取所有凭据的缓存余额信息（用于 Admin API）
    ///
    /// 返回每个凭据的缓存余额、缓存时间和 TTL
    pub fn get_all_cached_balances(&self) -> Vec<CachedBalanceInfo> {
        // 先获取 entries 的 ID 列表，避免同时持有两个锁
        let entry_ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).collect()
        };

        let cache = self.balance_cache.lock();
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        entry_ids
            .iter()
            .filter_map(|&id| {
                cache.get(&id).map(|cached| {
                    // 计算动态 TTL
                    let ttl_secs = if !cached.initialized {
                        // 未初始化的缓存，TTL 设为 0（已过期）
                        0
                    } else if cached.remaining < LOW_BALANCE_THRESHOLD {
                        BALANCE_TTL_LOW_BALANCE_SECS
                    } else if cached.recent_usage >= HIGH_FREQ_THRESHOLD {
                        BALANCE_TTL_HIGH_FREQ_SECS
                    } else {
                        BALANCE_TTL_LOW_FREQ_SECS
                    };

                    // 计算缓存时间的 Unix 毫秒时间戳
                    let elapsed_ms = cached.cached_at.elapsed().as_millis() as u64;
                    let cached_at_unix_ms = now_unix_ms.saturating_sub(elapsed_ms);

                    CachedBalanceInfo {
                        id,
                        remaining: cached.remaining,
                        cached_at: cached_at_unix_ms,
                        ttl_secs,
                    }
                })
            })
            .collect()
    }

    /// 尝试使用指定凭据获取有效 Token
    ///
    /// 使用双重检查锁定模式，确保同一时间只有一个刷新操作
    ///
    /// # Arguments
    /// * `id` - 凭据 ID，用于更新正确的条目
    /// * `credentials` - 凭据信息
    /// 强制使用指定凭据获取调用上下文（绕过调度）。
    /// 用于"对话测试"等需要锁定单凭据的场景。
    pub async fn acquire_context_for_credential(&self, id: u64) -> anyhow::Result<CallContext> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
        };
        self.try_ensure_token(id, &credentials).await
    }

    async fn try_ensure_token(
        &self,
        id: u64,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<CallContext> {
        // 获取配置快照（避免跨 await 持有读锁）
        let config = self.config.read().clone();

        let token_missing_or_truncated = |creds: &KiroCredentials| {
            if creds.is_api_key_credential() {
                return creds
                    .kiro_api_key
                    .as_deref()
                    .is_none_or(|t| t.trim().is_empty());
            }
            creds
                .access_token
                .as_deref()
                .is_none_or(|t| t.trim().is_empty() || t.ends_with("...") || t.contains("..."))
        };

        // 第一次检查（无锁）：快速判断是否需要刷新
        let needs_refresh = token_missing_or_truncated(credentials)
            || is_token_expired(credentials)
            || is_token_expiring_soon(credentials);

        let creds = if needs_refresh {
            // 获取刷新锁，确保同一时间只有一个刷新操作
            let _refresh_guard = self.refresh_lock_for(id);
            let _guard = _refresh_guard.lock().await;

            // 第二次检查：获取锁后重新读取凭据，因为其他请求可能已经完成刷新
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            if token_missing_or_truncated(&current_creds)
                || is_token_expired(&current_creds)
                || is_token_expiring_soon(&current_creds)
            {
                // 确实需要刷新
                if current_creds.is_api_key_credential() {
                    current_creds
                } else {
                    let proxy = self.effective_proxy_for_cred(&current_creds)?;
                    let new_creds =
                        match refresh_token_with_id(&current_creds, &config, proxy.as_ref(), id)
                            .await
                        {
                            Ok(creds) => {
                                let mut entries = self.entries.lock();
                                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                                    entry.refresh_failure_count = 0;
                                    if entry.disable_reason
                                        == Some(DisableReason::RefreshFailureLimit)
                                    {
                                        entry.disabled = false;
                                        entry.auto_heal_reason = None;
                                        entry.disable_reason = None;
                                    }
                                }
                                creds
                            }
                            Err(err) => {
                                if is_invalid_grant_error(&err) {
                                    self.record_disable_event(
                                        id,
                                        DisableReason::AuthenticationFailed,
                                        None,
                                        format!("Token 刷新失败（invalid_grant）: {}", err),
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    );
                                    self.mark_authentication_failed(id);
                                    tracing::warn!(
                                        credential_id = id,
                                        "凭据 Token 刷新失败（invalid_grant，已立即禁用）: {}",
                                        err
                                    );
                                    return Err(err);
                                }

                                let has_available = {
                                    let mut entries = self.entries.lock();
                                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                                        entry.refresh_failure_count += 1;
                                        let refresh_failure_count = entry.refresh_failure_count;
                                        if refresh_failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                                            entry.disabled = true;
                                            entry.auto_heal_reason =
                                                Some(AutoHealReason::TooManyFailures);
                                            entry.disable_reason =
                                                Some(DisableReason::RefreshFailureLimit);
                                        }
                                    }
                                    entries.iter().any(|e| !e.disabled && e.id != id)
                                };
                                tracing::warn!(
                                    credential_id = id,
                                    has_available,
                                    "凭据 Token 刷新失败: {}",
                                    err
                                );
                                return Err(err);
                            }
                        };

                    if is_token_expired(&new_creds) {
                        anyhow::bail!("刷新后的 Token 仍然无效或已过期");
                    }

                    // 更新凭据
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.credentials = new_creds.clone();
                            // 更新哈希缓存
                            entry.refresh_token_hash = credential_secret_hash(&new_creds);
                        }
                    }

                    // 回写凭据到文件（仅多凭据格式），失败只记录警告
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                    }

                    new_creds
                }
            } else {
                // 其他请求已经完成刷新，直接使用新凭据
                tracing::debug!("Token 已被其他请求刷新，跳过刷新");
                current_creds
            }
        } else {
            credentials.clone()
        };

        let token = if creds.is_api_key_credential() {
            creds
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("没有可用的 kiroApiKey"))?
        } else {
            creds
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?
        };

        // Enterprise IdC / Q Developer 的数据面必须带 profileArn，缺失时会被上游 403 拒绝。
        // 在此按需解析并写回凭据（首次解析后持久化，后续请求直接命中）。
        let creds = self
            .ensure_idc_profile_arn(id, creds, &config, &token)
            .await;

        // 从 entry 取实时指标 Arc，给请求路径使用
        let (in_flight, rpm) = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?;
            (entry.in_flight.clone(), entry.rpm.clone())
        };

        Ok(CallContext {
            id,
            credentials: creds,
            token,
            in_flight,
            rpm,
        })
    }

    /// 为缺 profileArn 的 IdC / SSO-OIDC 凭据解析 profileArn 并写回（持久化）。
    ///
    /// Enterprise（Q Developer）账号的数据面 generateAssistantResponse 必须携带 profileArn，
    /// 否则上游返回 403「User is not authorized to make this call.」。通过 ListAvailableProfiles
    /// 取真实 profileArn；失败时退回固定兜底 ARN（个人/BuilderId 场景）。非 IdC/SSO 或已有
    /// profileArn 的凭据原样返回。
    async fn ensure_idc_profile_arn(
        &self,
        id: u64,
        mut creds: KiroCredentials,
        config: &Config,
        token: &str,
    ) -> KiroCredentials {
        let needs_profile_arn = creds
            .profile_arn
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true);
        if !needs_profile_arn || creds.is_api_key_credential() {
            return creds;
        }
        // 与 IdeEndpoint::is_aws_sso_oidc_credentials 一致：idc/builder-id/iam，或带 clientId+clientSecret
        let auth_is_sso_oidc = creds
            .auth_method
            .as_deref()
            .map(|m| {
                m.eq_ignore_ascii_case("idc")
                    || m.eq_ignore_ascii_case("builder-id")
                    || m.eq_ignore_ascii_case("iam")
            })
            .unwrap_or(false)
            || (creds.client_id.is_some() && creds.client_secret.is_some());
        if !auth_is_sso_oidc {
            return creds;
        }

        // 去重：与 token 刷新共用同一把每凭据锁，避免新凭据首次并发的 N 个请求各探一遍
        // （惊群 + 首请求高延迟）。到这里 try_ensure_token 的刷新锁已释放，不会自锁。
        let refresh_lock = self.refresh_lock_for(id);
        let _guard = refresh_lock.lock().await;
        // 二次检查：可能已被并发的另一个请求解析并写回
        {
            let entries = self.entries.lock();
            if let Some(arn) = entries
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.credentials.profile_arn.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                creds.profile_arn = Some(arn.to_string());
                return creds;
            }
        }

        let proxy = match self.effective_proxy_for_cred(&creds) {
            Ok(p) => p,
            Err(_) => return creds,
        };

        // 跨区探测：账号的认证/OIDC region（凭据 region）未必是 Q Developer profile 所在的
        // 数据面 region——Enterprise 账号 profile 常在 eu-central-1，而认证在 us-east-1。
        // 逐个候选 region 调 ListAvailableProfiles，命中即用；解析到的 ARN 已含正确 region，
        // 后续数据面 host 由 effective_api_region 从 ARN 解析（见 credentials.rs）。
        let mut candidate_regions: Vec<String> =
            vec![creds.effective_api_region(config).to_string()];
        for r in PROFILE_PROBE_REGIONS {
            if !candidate_regions.iter().any(|x| x == r) {
                candidate_regions.push((*r).to_string());
            }
        }
        let mut resolved: Option<String> = None;
        for region in &candidate_regions {
            match list_available_profiles_in_region(&creds, config, token, proxy.as_ref(), region)
                .await
            {
                Ok(found) => {
                    tracing::info!(
                        credential_id = id,
                        region = %region,
                        "ListAvailableProfiles 解析到 profileArn"
                    );
                    resolved = Some(found);
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        credential_id = id,
                        region = %region,
                        error = %e,
                        "该 region 无 profile，继续探测下一个"
                    );
                }
            }
        }
        match resolved {
            Some(arn) => {
                // 只持久化「真实解析到」的 profileArn。
                creds.profile_arn = Some(arn.clone());
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials.profile_arn = Some(arn);
                    }
                }
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("IdC profileArn 解析后持久化失败（不影响本次请求）: {}", e);
                }
            }
            None => {
                // 两区都没解析到——无法区分「瞬时网络/代理抖动」与「确实无 profile」。
                // 本次请求用兜底 ARN 应急，但【不写回凭据、不持久化】，下次请求会重新探测，
                // 避免瞬时失败把凭据永久钉在错误兜底 ARN 上（企业号→永久 403 且无自愈）。
                tracing::warn!(
                    credential_id = id,
                    "所有候选 region 均未解析到 profileArn，本次用兜底 ARN（不持久化，下次重试）"
                );
                creds.profile_arn = Some(FALLBACK_IDC_PROFILE_ARN.to_string());
            }
        }
        creds
    }

    /// 标记指定凭据的 accessToken 失效（强制触发后续刷新）
    ///
    /// 用于处理上游返回「bearer token invalid」但本地 expiresAt 未及时更新的场景：
    /// - 清空 accessToken（避免继续复用无效 token）
    /// - 将 expiresAt 设为当前时间（确保 is_token_expired() 为 true）
    ///
    /// 返回是否找到并更新了该凭据。
    pub fn invalidate_access_token(&self, id: u64) -> bool {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };

        entry.credentials.access_token = None;
        entry.credentials.expires_at = Some(Utc::now().to_rfc3339());
        true
    }

    /// 将凭据列表回写到源文件
    ///
    /// 仅在以下条件满足时回写：
    /// - 源文件是多凭据格式（数组）
    /// - credentials_path 已设置
    ///
    /// 注意：调用方应确保适当的同步机制，避免并发写入导致数据丢失。
    ///
    /// # Returns
    /// - `Ok(true)` - 成功写入文件
    /// - `Ok(false)` - 跳过写入（非多凭据格式或无路径配置）
    /// - `Err(_)` - 写入失败
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        use anyhow::Context;

        // 串行化整段「取快照 → 全量写库」，避免并发 persist 用过期快照覆盖新改动。
        // 在持锁期间才取快照，保证写入的一定是当下最新的全量状态（最后提交者写最新态）。
        let _persist_guard = self.persist_lock.lock();

        // 收集要持久化的凭据快照（带"自动禁用不落盘"逻辑）
        let credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|e| !e.credentials.runtime_only)
                .map(|e| {
                    let mut cred = e.credentials.clone();
                    cred.canonicalize_auth_method();
                    // 仅持久化手动禁用状态，自动禁用（失败阈值/额度用尽等）不落盘
                    cred.disabled = e.disable_reason == Some(DisableReason::Manual);
                    cred
                })
                .collect()
        };

        // 仅 SQLite store 路径（JSON 已废弃）
        let store = self
            .store
            .read()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("SQLite store 未注入，无法持久化凭据"))?;
        store
            .replace_all_credentials(&credentials)
            .context("写入 SQLite 凭据失败")?;
        Ok(true)
    }

    /// 获取缓存目录（凭据文件所在目录）
    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.credentials_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 统计数据文件路径
    fn stats_path(&self) -> Option<PathBuf> {
        self.cache_dir().map(|d| d.join("kiro_stats.json"))
    }

    /// 从磁盘加载统计数据并应用到当前条目
    fn load_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return, // 首次运行时文件不存在
        };

        let stats: HashMap<String, StatsEntry> = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("解析统计缓存失败，将忽略: {}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.error_count = s.error_count;
                entry
                    .rate_limit_count
                    .store(s.rate_limit_count, Ordering::Relaxed);
                entry.last_used_at = s.last_used_at.clone();
                entry.model_usage = s.model_usage.clone();
                entry.recent_outcomes = decode_recent_outcomes(&s.recent_outcomes);
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        self.stats_dirty.store(false, Ordering::Relaxed);
        tracing::info!("已从缓存加载 {} 条统计数据", stats.len());
    }

    /// 将当前统计数据持久化到磁盘
    fn save_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let stats: HashMap<String, StatsEntry> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    (
                        e.id.to_string(),
                        StatsEntry {
                            success_count: e.success_count,
                            error_count: e.error_count,
                            rate_limit_count: e.rate_limit_count.load(Ordering::Relaxed),
                            last_used_at: e.last_used_at.clone(),
                            model_usage: e.model_usage.clone(),
                            recent_outcomes: encode_recent_outcomes(&e.recent_outcomes),
                        },
                    )
                })
                .collect()
        };

        match serde_json::to_string_pretty(&stats) {
            Ok(json) => {
                // 原子写入：先写临时文件，再重命名
                let tmp_path = path.with_extension("json.tmp");
                match std::fs::write(&tmp_path, json) {
                    Ok(_) => {
                        if let Err(e) = std::fs::rename(&tmp_path, &path) {
                            tracing::warn!("原子重命名统计缓存失败: {}", e);
                            let _ = std::fs::remove_file(&tmp_path);
                        } else {
                            *self.last_stats_save_at.lock() = Some(Instant::now());
                            self.stats_dirty.store(false, Ordering::Relaxed);
                        }
                    }
                    Err(e) => tracing::warn!("写入临时统计文件失败: {}", e),
                }
            }
            Err(e) => tracing::warn!("序列化统计数据失败: {}", e),
        }
    }

    /// 标记统计数据已更新，并按 debounce 策略决定是否立即落盘
    fn save_stats_debounced(&self) {
        self.stats_dirty.store(true, Ordering::Relaxed);

        let should_flush = {
            let last = *self.last_stats_save_at.lock();
            match last {
                Some(last_saved_at) => last_saved_at.elapsed() >= STATS_SAVE_DEBOUNCE,
                None => true,
            }
        };

        if should_flush {
            self.save_stats();
        }
    }

    /// 取某凭据专属的 token 刷新锁（不存在则创建）。
    /// 保证同一凭据不会并发重复刷新，不同凭据可并行刷新。
    fn refresh_lock_for(&self, id: u64) -> Arc<TokioMutex<()>> {
        let mut locks = self.refresh_locks.lock();
        locks.entry(id).or_default().clone()
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_success(&self, id: u64) {
        // 重置 MODEL_TEMPORARILY_UNAVAILABLE 计数器
        self.model_unavailable_count.store(0, Ordering::SeqCst);

        // 记录使用次数（用于动态 TTL）
        self.record_usage(id);

        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failure_count = 0;
                entry.success_count += 1;
                push_recent_outcome(&mut entry.recent_outcomes, OUTCOME_SUCCESS);
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        self.save_stats_debounced();
    }

    /// 记录一次请求的用量到对应凭据（按模型累加），用于「产出价值」统计。
    ///
    /// 只存原始计数；价值在展示时按当前定价实时换算。失败不影响主流程。
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 ApiCallResult.credential_id）
    /// * `model` - 客户端请求的模型名
    /// * `input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_write_tokens` - 本次 token 用量
    /// * `credit_usage` - 上游 meteringEvent.usage（积分），无则传 0
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
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                let mu = entry.model_usage.entry(model.to_string()).or_default();
                mu.input_tokens += input_tokens.max(0) as u64;
                mu.output_tokens += output_tokens.max(0) as u64;
                mu.cache_read_tokens += cache_read_tokens.max(0) as u64;
                mu.cache_write_tokens += cache_write_tokens.max(0) as u64;
                if credit_usage.is_finite() && credit_usage > 0.0 {
                    mu.credit_usage += credit_usage;
                }
                mu.calls += 1;
            } else {
                return;
            }
        }
        // 记入时间窗口账本（用于「预计可撑时长」估算）
        if credit_usage.is_finite() && credit_usage > 0.0 {
            let now = Instant::now();
            let mut ledger = self.credit_ledger.lock();
            ledger.push_back((now, credit_usage));
            while let Some(&(t, _)) = ledger.front() {
                if now.duration_since(t) > CREDIT_LEDGER_MAX_WINDOW {
                    ledger.pop_front();
                } else {
                    break;
                }
            }
        }
        self.save_stats_debounced();
    }

    /// 最近 window 时间内的点数消耗合计（内存账本，重启清零）
    pub fn recent_credit_usage(&self, window: std::time::Duration) -> f64 {
        let window = window.min(CREDIT_LEDGER_MAX_WINDOW);
        let now = Instant::now();
        let ledger = self.credit_ledger.lock();
        ledger
            .iter()
            .rev()
            .take_while(|(t, _)| now.duration_since(*t) <= window)
            .map(|(_, c)| c)
            .sum()
    }

    /// 汇总启用中凭据的缓存余额：(剩余点数合计, 有余额缓存的凭据数, 启用中凭据总数)。
    /// 只统计已初始化的缓存条目，未查询过余额的凭据不计入合计。
    pub fn total_remaining_balance(&self) -> (f64, usize, usize) {
        let enabled_ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|e| !e.disabled)
                .map(|e| e.id)
                .collect()
        };
        let enabled_total = enabled_ids.len();
        let cache = self.balance_cache.lock();
        let mut sum = 0.0;
        let mut counted = 0;
        for id in enabled_ids {
            if let Some(cached) = cache.get(&id)
                && cached.initialized
            {
                sum += cached.remaining.max(0.0);
                counted += 1;
            }
        }
        (sum, counted, enabled_total)
    }

    /// 报告指定凭据 API 调用失败
    ///
    /// 增加失败计数，达到阈值时禁用凭据
    /// 返回是否还有可用凭据可以重试
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_failure(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            entry.failure_count += 1;
            entry.error_count += 1;
            push_recent_outcome(&mut entry.recent_outcomes, OUTCOME_ERROR);
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            let failure_count = entry.failure_count;

            tracing::warn!(
                "凭据 #{} API 调用失败（{}/{}）",
                id,
                failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                entry.disabled = true;
                entry.auto_heal_reason = Some(AutoHealReason::TooManyFailures);
                entry.disable_reason = Some(DisableReason::FailureLimit);
                tracing::error!("凭据 #{} 已连续失败 {} 次，已被禁用", id, failure_count);

                // 移除该凭据的亲和性绑定
                drop(entries);
                self.affinity.remove_by_credential(id);

                let entries = self.entries.lock();
                return entries.iter().any(|e| !e.disabled);
            }

            // 检查是否还有可用凭据
            entries.iter().any(|e| !e.disabled)
        };
        self.save_stats_debounced();
        result
    }

    /// 报告指定凭据额度已用尽
    ///
    /// 用于处理 402 Payment Required 且 reason 为 `MONTHLY_REQUEST_COUNT` 的场景：
    /// - 立即禁用该凭据（不等待连续失败阈值）
    /// - 切换到下一个可用凭据继续重试
    /// - 返回是否还有可用凭据
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            // 开启「允许超额使用」时不禁用，由上游决定是否实际放行
            if entry.credentials.allow_overuse {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::warn!("凭据 #{} 额度用尽但已开启允许超额使用，保持启用状态", id);
                return entries.iter().any(|e| !e.disabled);
            }

            entry.disabled = true;
            entry.auto_heal_reason = Some(AutoHealReason::QuotaExceeded);
            entry.disable_reason = Some(DisableReason::QuotaExceeded);
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            // 设为阈值，便于在管理面板中直观看到该凭据已不可用
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!("凭据 #{} 额度已用尽（MONTHLY_REQUEST_COUNT），已被禁用", id);

            entries.iter().any(|e| !e.disabled)
        };
        self.save_stats_debounced();
        result
    }

    /// 报告 MODEL_TEMPORARILY_UNAVAILABLE 错误
    ///
    /// 累计达到阈值后禁用所有凭据，5分钟后自动恢复
    /// 返回是否触发了全局禁用
    ///
    /// 若 `Config.model_unavailable_breaker_enabled = false`，直接返回 false 且不计数，
    /// 由调用方继续按单凭据故障转移/重试路径处理。
    pub fn report_model_unavailable(&self) -> bool {
        if !self.config.read().model_unavailable_breaker_enabled {
            tracing::debug!("MODEL_TEMPORARILY_UNAVAILABLE 全局熔断已关闭，跳过计数");
            return false;
        }

        let count = self.model_unavailable_count.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::warn!(
            "MODEL_TEMPORARILY_UNAVAILABLE 错误（{}/{}）",
            count,
            MODEL_UNAVAILABLE_THRESHOLD
        );

        if count >= MODEL_UNAVAILABLE_THRESHOLD {
            self.disable_all_credentials(DisableReason::ModelUnavailable);
            true
        } else {
            false
        }
    }

    /// 热更新「MODEL_TEMPORARILY_UNAVAILABLE 全局熔断开关」
    ///
    /// 关闭时会重置内部累计计数，避免开关再次开启后立刻触发。
    pub fn update_model_unavailable_breaker_enabled(&self, enabled: bool) {
        self.config.write().model_unavailable_breaker_enabled = enabled;
        if !enabled {
            self.model_unavailable_count.store(0, Ordering::SeqCst);
        }
    }

    /// 禁用所有凭据
    fn disable_all_credentials(&self, reason: DisableReason) {
        let mut entries = self.entries.lock();
        let mut recovery_time = self.global_recovery_time.lock();

        for entry in entries.iter_mut() {
            if !entry.disabled {
                entry.disabled = true;
                entry.disable_reason = Some(reason);
            }
        }

        // 设置恢复时间
        let recover_at = Utc::now() + Duration::minutes(GLOBAL_DISABLE_RECOVERY_MINUTES);
        *recovery_time = Some(recover_at);

        tracing::error!(
            "所有凭据已被禁用（原因: {:?}），将于 {} 自动恢复",
            reason,
            recover_at.format("%H:%M:%S")
        );
    }

    /// 检查并执行自动恢复
    ///
    /// 如果已到恢复时间，恢复因 ModelUnavailable 禁用的凭据
    /// 余额不足的凭据不会被恢复
    ///
    /// 返回是否执行了恢复
    pub fn check_and_recover(&self) -> bool {
        // 额度用尽（QuotaExceeded）按时自动复检，独立于全局熔断恢复：到窗口即重新启用，
        // 下次请求复检额度。覆盖瞬时 402 与月度额度重置，避免只能人工/重启恢复。
        let quota_recovered = self.recover_expired_quota();

        let should_recover = {
            let recovery_time = self.global_recovery_time.lock();
            recovery_time.map(|t| Utc::now() >= t).unwrap_or(false)
        };

        if !should_recover {
            return quota_recovered;
        }

        let mut entries = self.entries.lock();
        let mut recovery_time = self.global_recovery_time.lock();
        let mut recovered_count = 0;

        for entry in entries.iter_mut() {
            // 只恢复因 ModelUnavailable 禁用的凭据，余额不足的不恢复
            if entry.disabled && entry.disable_reason == Some(DisableReason::ModelUnavailable) {
                entry.disabled = false;
                entry.disable_reason = None;
                entry.failure_count = 0;
                recovered_count += 1;
            }
        }

        // 重置全局状态
        *recovery_time = None;
        self.model_unavailable_count.store(0, Ordering::SeqCst);

        if recovered_count > 0 {
            tracing::info!("已自动恢复 {} 个凭据", recovered_count);
        }

        recovered_count > 0 || quota_recovered
    }

    /// 复检并恢复「额度用尽」（QuotaExceeded）到达自动复检窗口的凭据。
    /// 以 `last_used_at`（在 report_quota_exhausted 时被设为禁用时刻）为基准，
    /// 超过 [`QUOTA_AUTO_RECOVERY_HOURS`] 即重新启用；开启 allow_overuse 的凭据不在此列
    /// （它们本就不因额度被禁用）。返回是否恢复了任意凭据。
    fn recover_expired_quota(&self) -> bool {
        let now = Utc::now();
        let mut entries = self.entries.lock();
        let mut recovered = 0;
        for entry in entries.iter_mut() {
            if !entry.disabled
                || entry.disable_reason != Some(DisableReason::QuotaExceeded)
                || entry.credentials.allow_overuse
            {
                continue;
            }
            // 无禁用时间戳时保守地允许复检（避免永久卡死）
            let due = entry
                .last_used_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| now >= t.with_timezone(&Utc) + Duration::hours(QUOTA_AUTO_RECOVERY_HOURS))
                .unwrap_or(true);
            if due {
                entry.disabled = false;
                entry.disable_reason = None;
                entry.auto_heal_reason = None;
                entry.failure_count = 0;
                // 重置 LRU 时间戳为「刚用过」：额度可能仍未真正 reset（月度额度要几周），
                // 若保留旧的（1h 前）时间戳，选号器会把它当「等最久」优先选中 → 每次复检都先
                // 命中这个死号拿 402、浪费一次用户请求。置为 now 让它排到队尾，仅在健康号忙/
                // 故障转移时才被探测。
                entry.last_acquired_at = Some(std::time::Instant::now());
                recovered += 1;
                tracing::info!(
                    "凭据 #{} 额度复检窗口到期，自动重新启用（下次请求会复检额度）",
                    entry.id
                );
            }
        }
        recovered > 0
    }

    /// 累计 429 触发次数（在 provider 收到 429 响应时调用；随统计缓存持久化）
    pub fn report_rate_limit(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry
                    .rate_limit_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                push_recent_outcome(&mut entry.recent_outcomes, OUTCOME_RATE_LIMIT);
            }
        }
        self.save_stats_debounced();
    }

    /// 记录一次「收到上游错误响应」：凭据级累计错误 +1（随统计缓存持久化）。
    ///
    /// 与 `report_failure` 的区别：不动连续失败计数（failure_count）、不触发禁用，
    /// 仅用于统计口径（凭据「统计」列的失败数）。调用约定：
    /// - 只在确实收到上游错误响应时调用（网络错误/本地错误不计入）
    /// - 429 走 `report_rate_limit`，普通 401/403 走 `report_failure`，均不重复调用本方法；
    ///   例外：bearer 失效触发自动刷新重试的首个 401/403 用本方法计数（不推进禁用计数）
    pub fn record_upstream_error(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.error_count += 1;
                push_recent_outcome(&mut entry.recent_outcomes, OUTCOME_ERROR);
            }
        }
        self.save_stats_debounced();
    }

    /// 标记凭据为代理槽资源耗尽（绑定代理过期且无可替换代理）。
    /// 进入 ProxyExhausted 短冷却，由后台轮换或管理员补充代理后自动恢复。
    pub fn report_proxy_exhausted(&self, id: u64) {
        self.cooldown_manager
            .set_cooldown(id, CooldownReason::ProxyExhausted);
        tracing::warn!("凭据 #{} 已标记为代理资源耗尽（ProxyExhausted）", id);
    }

    /// 标记凭据为「无可用代理」并禁用（所绑代理失效且池中无可替换代理时调用）。
    /// 管理员补充/重置代理后可手动重新启用并绑定。
    pub fn mark_proxy_unavailable(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.disabled = true;
                entry.auto_heal_reason = None;
                entry.disable_reason = Some(DisableReason::ProxyUnavailable);
                tracing::warn!("凭据 #{} 已因无可用代理被禁用", id);
            }
        }
        self.affinity.remove_by_credential(id);
        self.record_disable_event(
            id,
            DisableReason::ProxyUnavailable,
            None,
            "所绑代理不可用且池中无可替换代理，凭据被禁用".to_string(),
            None,
            None,
            None,
            None,
            None,
        );
    }

    /// 上报一次「经凭据所绑代理成功收到上游响应」：清零该代理的连续网络失败计数。
    /// 未启用代理池或凭据未绑定代理时为 no-op。
    pub fn report_proxy_network_success(&self, id: u64) {
        let Some(pool) = self.proxy_pool.read().clone() else {
            return;
        };
        let slot_id = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.credentials.proxy_slot_id.clone())
        };
        if let Some(slot_id) = slot_id {
            pool.record_network_success(&slot_id);
        }
    }

    /// 上报一次「经凭据所绑代理的网络层失败」（未收到上游响应）。
    ///
    /// 同一代理连续失败达到阈值（`proxy_failure_threshold`，0 = 关闭）时：
    /// 1. 把该代理标记为不可用（类别 NetworkFailure，持久化）
    /// 2. 该代理上绑定的所有凭据立即换绑到可用代理
    /// 3. 池中无可用代理的凭据：解绑并禁用（DisableReason::ProxyUnavailable）
    ///
    /// 未启用代理池或凭据未绑定代理时为 no-op。
    pub fn report_proxy_network_failure(&self, id: u64) {
        let Some(pool) = self.proxy_pool.read().clone() else {
            return;
        };
        let threshold = self.config.read().proxy_failure_threshold;
        if threshold == 0 {
            return;
        }
        let slot_id = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.credentials.proxy_slot_id.clone())
        };
        let Some(slot_id) = slot_id else {
            return;
        };

        let count = pool.record_network_failure(&slot_id);
        tracing::debug!(
            credential_id = id,
            slot_id = %slot_id,
            count,
            threshold,
            "代理网络层失败计数"
        );
        // 仅在恰好达到阈值时触发一次（计数在写锁内自增，并发下只有一个调用命中）
        if count != threshold {
            return;
        }

        tracing::warn!(
            slot_id = %slot_id,
            threshold,
            "代理连续 {} 次网络请求失败，标记为不可用并为所绑凭据换绑",
            count
        );
        let reason = format!("连续 {} 次网络请求失败，自动标记不可用", count);
        if let Err(e) = pool.set_disabled(
            &slot_id,
            true,
            Some(crate::kiro::proxy_pool::ProxyDisabledCategory::NetworkFailure),
            Some(reason.clone()),
        ) {
            tracing::warn!(slot_id = %slot_id, "标记代理不可用失败: {}", e);
            return;
        }
        pool.push_alert(
            crate::kiro::proxy_pool::AlertLevel::Error,
            format!("代理 {} {}", slot_id, reason),
        );
        self.migrate_creds_off_proxy(&pool, &slot_id);
    }

    /// 把某代理上绑定的所有凭据换绑到可用代理；无可用代理的凭据解绑并禁用。
    ///
    /// 供「连续网络失败自动禁用」与「管理员手动禁用代理」共用。
    pub fn migrate_creds_off_proxy(
        &self,
        pool: &Arc<crate::kiro::proxy_pool::ProxyPool>,
        slot_id: &str,
    ) {
        use crate::kiro::proxy_pool::AlertLevel;

        let bound = match pool.get(slot_id) {
            Some(e) => e.bound_credential_ids,
            None => return,
        };
        let warning_hours = self.config.read().proxy_expiry_warning_hours;
        for cred_id in bound {
            // 优先挑剩余有效期充足的候选；实在没有则放宽到"未过期即可"（应急）
            let candidate = pool
                .find_rotation_candidate(slot_id, warning_hours, cred_id)
                .or_else(|| pool.find_rotation_candidate(slot_id, 0, cred_id));
            match candidate {
                Some(new_slot) => {
                    if let Err(e) = pool.migrate_binding(cred_id, slot_id, &new_slot) {
                        tracing::warn!(
                            credential_id = cred_id,
                            "代理失效换绑：迁移绑定失败: {}",
                            e
                        );
                        pool.push_alert(
                            AlertLevel::Error,
                            format!("凭据 #{} 代理失效换绑失败: {}", cred_id, e),
                        );
                        continue;
                    }
                    if let Err(e) = self.set_proxy_slot(cred_id, Some(new_slot.clone())) {
                        tracing::warn!(
                            credential_id = cred_id,
                            "代理失效换绑：更新凭据 proxy_slot_id 失败: {}",
                            e
                        );
                        // 写回失败则回滚池内迁移，避免不一致
                        let _ = pool.migrate_binding(cred_id, &new_slot, slot_id);
                        continue;
                    }
                    tracing::info!(
                        credential_id = cred_id,
                        from = %slot_id,
                        to = %new_slot,
                        "代理失效换绑：凭据已切换到可用代理"
                    );
                    pool.push_alert(
                        AlertLevel::Info,
                        format!(
                            "凭据 #{} 已从失效代理 {} 换绑到 {}",
                            cred_id, slot_id, new_slot
                        ),
                    );
                }
                None => {
                    let _ = pool.unbind(slot_id, cred_id);
                    let _ = self.set_proxy_slot(cred_id, None);
                    self.mark_proxy_unavailable(cred_id);
                    pool.push_alert(
                        AlertLevel::Error,
                        format!(
                            "凭据 #{} 所绑代理 {} 失效且池中无可用代理，凭据已禁用",
                            cred_id, slot_id
                        ),
                    );
                }
            }
        }
    }

    /// 标记凭据为认证失败（如 invalid_grant，不会被自动恢复）
    pub fn mark_authentication_failed(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.disabled = true;
            entry.auto_heal_reason = None;
            entry.disable_reason = Some(DisableReason::AuthenticationFailed);
            tracing::warn!("凭据 #{} 已标记为认证失败", id);
        }
        drop(entries);
        self.affinity.remove_by_credential(id);
    }

    /// 标记凭据为账户暂停（不会被自动恢复）
    pub fn mark_account_suspended(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.disabled = true;
            entry.auto_heal_reason = None;
            entry.disable_reason = Some(DisableReason::AccountSuspended);
            tracing::warn!("凭据 #{} 已标记为账户暂停", id);
        }
        drop(entries);
        self.affinity.remove_by_credential(id);
    }

    /// 标记凭据为余额不足（不会被自动恢复）
    ///
    /// 若该凭据开启了「允许超额使用」(`allow_overuse=true`)，则跳过禁用，
    /// 由上游决定是否实际放行（前提是上游账号侧也开启了超额计费）。
    pub fn mark_insufficient_balance(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if entry.credentials.allow_overuse {
                tracing::info!("凭据 #{} 余额不足但已开启允许超额使用，保持启用状态", id);
                return;
            }
            entry.disabled = true;
            entry.auto_heal_reason = None; // 清除自愈原因，防止被自愈循环错误恢复
            entry.disable_reason = Some(DisableReason::InsufficientBalance);
            tracing::warn!("凭据 #{} 已标记为余额不足", id);
        }
    }

    /// 设置凭据「允许超额使用」开关（Admin API）
    pub fn set_allow_overuse(&self, id: u64, allow: bool) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.allow_overuse = allow;
            // 开启时，若当前因余额不足被禁用，立刻解除
            if allow
                && entry.disabled
                && entry.disable_reason == Some(DisableReason::InsufficientBalance)
            {
                entry.disabled = false;
                entry.disable_reason = None;
                entry.auto_heal_reason = None;
                tracing::info!("凭据 #{} 已开启允许超额使用，解除余额不足禁用", id);
            }
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 获取全局恢复时间（用于 Admin API）
    #[allow(dead_code)]
    pub fn get_recovery_time(&self) -> Option<DateTime<Utc>> {
        *self.global_recovery_time.lock()
    }

    /// 获取使用额度信息
    #[allow(dead_code)]
    pub async fn get_usage_limits(&self) -> anyhow::Result<UsageLimitsResponse> {
        let config = self.config.read().clone();
        let ctx = self.acquire_context().await?;
        let proxy = self.effective_proxy_for_cred(&ctx.credentials)?;
        get_usage_limits(&ctx.credentials, &config, &ctx.token, proxy.as_ref()).await
    }

    /// 初始化所有凭据的余额缓存
    ///
    /// 启动时顺序查询所有凭据的余额，每次间隔 0.5 秒避免触发限流。
    /// 查询失败的凭据会被跳过（保持 initialized: false）。
    ///
    /// # 返回
    /// - 成功初始化的凭据数量
    #[allow(dead_code)]
    pub async fn initialize_balances(&self) -> usize {
        let credential_ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|e| !e.disabled)
                .map(|e| e.id)
                .collect()
        };

        if credential_ids.is_empty() {
            tracing::info!("无可用凭据，跳过余额初始化");
            return 0;
        }

        let total = credential_ids.len();
        let concurrency = (self.config.read().balance_refresh_concurrency.max(1) as usize).min(256);
        tracing::info!(
            "正在初始化 {} 个凭据的余额（并发 {}）...",
            total,
            concurrency
        );

        // 有界并发查询：每个凭据走各自代理出口，并发刷新不会撞同一 IP 的上游限流。
        use futures::stream::StreamExt;
        let results = futures::stream::iter(credential_ids.into_iter())
            .map(|id| async move {
                match self.get_usage_limits_for(id).await {
                    Ok(limits) => {
                        // 上游返回 200 但无 usageBreakdownList 时，usage_limit()/current_usage() 都会回退到 0，
                        // 「数据缺失」不等于「余额为零」，据此禁用（InsufficientBalance 不可自愈）会误伤全部凭据。
                        // 此处保持启用并跳过判定，真实欠费会在后续请求由上游 402 等权威信号处理。
                        if !limits.has_usage_data() {
                            tracing::warn!(
                                "凭据 #{} 余额响应缺少 usageBreakdownList，跳过余额判定（保持启用）",
                                id
                            );
                            return true;
                        }
                        let used = limits.current_usage();
                        let limit = limits.usage_limit();
                        let remaining = (limit - used).max(0.0);
                        self.update_balance_cache(id, remaining);

                        // 余额小于 1 时自动禁用凭据（开启允许超额则跳过）
                        if remaining < 1.0 {
                            let mut entries = self.entries.lock();
                            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                                if entry.credentials.allow_overuse {
                                    tracing::info!(
                                        "凭据 #{} 余额不足 ({:.2}) 但已开启允许超额使用，保持启用",
                                        id,
                                        remaining
                                    );
                                } else {
                                    entry.disabled = true;
                                    entry.disable_reason =
                                        Some(DisableReason::InsufficientBalance);
                                    tracing::warn!(
                                        "凭据 #{} 余额不足 ({:.2})，已自动禁用",
                                        id,
                                        remaining
                                    );
                                }
                            }
                        } else {
                            tracing::info!("凭据 #{} 余额初始化成功: {:.2}", id, remaining);
                        }
                        true
                    }
                    Err(e) => {
                        tracing::warn!("凭据 #{} 余额查询失败: {}", id, e);
                        false
                    }
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<bool>>()
            .await;

        let success_count = results.into_iter().filter(|ok| *ok).count();
        tracing::info!("余额初始化完成: {}/{} 成功", success_count, total);
        success_count
    }

    // ========================================================================
    // Admin API 方法
    // ========================================================================

    /// 获取管理器状态快照（用于 Admin API）
    pub fn snapshot(&self) -> ManagerSnapshot {
        let entries = self.entries.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| {
                    // 使用缓存的哈希，如果不存在则计算并缓存
                    let hash = e
                        .refresh_token_hash
                        .clone()
                        .or_else(|| credential_secret_hash(&e.credentials));

                    let (cooldown_reason, cooldown_remaining_secs) =
                        match self.cooldown_manager.check_cooldown(e.id) {
                            Some((reason, remaining)) => {
                                (Some(format!("{:?}", reason)), Some(remaining.as_secs()))
                            }
                            None => (None, None),
                        };

                    CredentialEntrySnapshot {
                        id: e.id,
                        priority: e.credentials.priority,
                        disabled: e.disabled,
                        disable_reason: e.disable_reason,
                        failure_count: e.failure_count,
                        refresh_failure_count: e.refresh_failure_count,
                        auth_method: if e.credentials.is_api_key_credential() {
                            // 只填了 kiroApiKey 未写 authMethod 时也归一为 api_key
                            Some("api_key".to_string())
                        } else {
                            e.credentials.auth_method.as_deref().map(|m| {
                                if m.eq_ignore_ascii_case("builder-id")
                                    || m.eq_ignore_ascii_case("iam")
                                {
                                    "idc".to_string()
                                } else {
                                    m.to_string()
                                }
                            })
                        },
                        has_profile_arn: e.credentials.profile_arn.is_some(),
                        expires_at: e.credentials.expires_at.clone(),
                        refresh_token_hash: hash,
                        email: e.credentials.email.clone(),
                        subscription_title: e.credentials.subscription_title.clone(),
                        success_count: e.success_count,
                        error_count: e.error_count,
                        last_used_at: e.last_used_at.clone(),
                        region: e.credentials.region.clone(),
                        api_region: e.credentials.api_region.clone(),
                        endpoint: e.credentials.endpoint.clone(),
                        proxy_slot_id: e.credentials.proxy_slot_id.clone(),
                        overage_status: e.credentials.last_overage_status.clone(),
                        in_flight: e.in_flight.load(Ordering::Relaxed),
                        rpm: e.rpm.rpm_60s(),
                        rate_limit_count: e.rate_limit_count.load(Ordering::Relaxed),
                        allow_overuse: e.credentials.allow_overuse,
                        cooldown_reason,
                        cooldown_remaining_secs,
                        credential_rpm: e.credentials.rpm,
                        model_usage: e.model_usage.clone(),
                        recent_outcomes: encode_recent_outcomes(&e.recent_outcomes),
                    }
                })
                .collect(),
            total: entries.len(),
            available,
        }
    }

    /// 设置凭据禁用状态（Admin API）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.disabled = disabled;
            if !disabled {
                // 启用时重置失败计数
                entry.failure_count = 0;
                entry.auto_heal_reason = None;
                entry.disable_reason = None;
            } else {
                entry.auto_heal_reason = Some(AutoHealReason::Manual);
                entry.disable_reason = Some(DisableReason::Manual);
            }
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.priority = priority;
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据所绑代理槽 ID（Admin API / 后台轮换）
    ///
    /// 同时清除该凭据的 ProxyExhausted 冷却（如有）。
    pub fn set_proxy_slot(&self, id: u64, slot: Option<String>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.proxy_slot_id = slot;
        }
        // 解除可能存在的 ProxyExhausted 冷却
        self.cooldown_manager.clear_cooldown(id);
        self.persist_credentials()?;
        Ok(())
    }

    /// 列出所有"当前已绑定代理槽"的凭据 (id, slot_id)，用于后台轮换扫描
    pub fn list_credential_proxy_bindings(&self) -> Vec<(u64, String)> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter_map(|e| e.credentials.proxy_slot_id.clone().map(|slot| (e.id, slot)))
            .collect()
    }

    /// 设置凭据 Region（Admin API）
    /// Patch 式更新 Region / API Region：
    /// - 外层 `None` = 该字段不变
    /// - 外层 `Some(None)` = 清除该字段
    /// - 外层 `Some(Some(s))` = 设置为 s
    ///
    /// 两个字段独立，可以只改其中一个，避免批量操作误清空另一个。
    pub fn set_region(
        &self,
        id: u64,
        region: Option<Option<String>>,
        api_region: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        if region.is_none() && api_region.is_none() {
            // 啥都没传，no-op
            return Ok(());
        }
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if let Some(patch) = region {
                entry.credentials.region = patch;
            }
            if let Some(patch) = api_region {
                entry.credentials.api_region = patch;
            }
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据邮箱（Admin API）
    pub fn set_email(&self, id: u64, email: Option<String>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.email = email;
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据级 RPM 上限（Admin API；None 表示清除，沿用全局 credentialRpm）
    pub fn set_credential_rpm_for(&self, id: u64, rpm: Option<u32>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.rpm = rpm.filter(|&v| v > 0);
        }

        // 计算该凭据的 min_interval 并应用到 rate_limiter；None 表示清除覆盖
        let interval = rpm
            .filter(|&v| v > 0)
            .map(|v| (60_000u64 / v as u64).max(1));
        self.rate_limiter.set_credential_min_interval(id, interval);

        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据 endpoint（Admin API）
    pub fn set_endpoint(&self, id: u64, endpoint: Option<String>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.endpoint = endpoint;
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 仅清空所有凭据的累计 429 计数（success/error/用量统计不动），立即落盘。
    /// 返回处理的凭据数。
    pub fn reset_all_rate_limit_counts(&self) -> usize {
        let n = {
            let entries = self.entries.lock();
            for entry in entries.iter() {
                entry.rate_limit_count.store(0, Ordering::Relaxed);
            }
            entries.len()
        };
        // 立即落盘，避免重启又恢复成旧值
        self.save_stats();
        n
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    /// 清空所有凭据的累计统计（不影响连续失败计数 / 禁用状态 / 邮箱等真值）：
    /// - success_count → 0
    /// - rate_limit_count → 0
    /// - last_used_at 保留（真实事件时间戳）
    pub fn reset_all_credential_stats(&self) {
        {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                entry.success_count = 0;
                entry.error_count = 0;
                entry.rate_limit_count.store(0, Ordering::Relaxed);
                entry.model_usage.clear();
                entry.recent_outcomes.clear();
            }
        }
        // 立即落盘，避免重启又恢复成旧值
        self.save_stats();
    }

    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disable_reason == Some(DisableReason::InvalidConfig) {
                anyhow::bail!(
                    "凭据 #{} 因配置无效被禁用（如缺少 kiroApiKey），请修正配置后重启服务",
                    id
                );
            }
            entry.failure_count = 0;
            entry.refresh_failure_count = 0;
            entry.disabled = false;
            entry.auto_heal_reason = None;
            entry.disable_reason = None;
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 拉取该凭据支持的模型列表（上游 ListAvailableModels）
    pub async fn list_available_models_for(&self, id: u64) -> anyhow::Result<serde_json::Value> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 取（必要时刷新的）token
        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 kiroApiKey"))?
        } else if is_token_expired(&credentials) || is_token_expiring_soon(&credentials) {
            let _refresh_guard = self.refresh_lock_for(id);
            let _guard = _refresh_guard.lock().await;
            let proxy = self.effective_proxy_for_cred(&credentials)?;
            let new_creds =
                refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await?;
            {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    entry.credentials = new_creds.clone();
                    entry.refresh_token_hash = credential_secret_hash(&new_creds);
                }
            }
            let _ = self.persist_credentials();
            new_creds
                .access_token
                .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
        } else {
            credentials
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
        };

        let machine_id = machine_id::generate_from_credentials(&credentials, &config)
            .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
        let endpoint = endpoint_for_credentials(&credentials, &config)?;
        let ctx = RequestContext {
            credentials: &credentials,
            token: &token,
            machine_id: &machine_id,
            config: &config,
        };
        let parts = endpoint.list_models_parts(&ctx)?;
        let proxy = self.effective_proxy_for_cred(&credentials)?;
        let client = build_client(proxy.as_ref(), 30, config.tls_backend)?;
        let mut req = client.get(&parts.url);
        for (name, value) in parts.headers {
            req = req.header(name, value);
        }
        let response = req.send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("ListAvailableModels 返回 {}: {}", status, body);
        }
        let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            anyhow::anyhow!("解析 ListAvailableModels 响应失败: {} (body={})", e, body)
        })?;
        Ok(value)
    }

    /// 设置上游 overage 偏好（开关超额计费）
    ///
    /// 流程：
    /// 1. 取凭据 + token（必要时刷新）
    /// 2. 走代理池有效代理调用上游 setUserPreference
    /// 3. 上游错误若包含 "reached the limit for overages" → 自动 mark_insufficient_balance
    /// 4. 成功后写 last_overage_status 缓存
    pub async fn set_overage_for(&self, id: u64, status: &str) -> anyhow::Result<String> {
        let normalized = status.trim().to_ascii_uppercase();
        if normalized != "ENABLED" && normalized != "DISABLED" {
            anyhow::bail!("overage_status 必须是 ENABLED 或 DISABLED");
        }

        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 取（必要时刷新的）token
        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 kiroApiKey"))?
        } else if is_token_expired(&credentials) || is_token_expiring_soon(&credentials) {
            let _refresh_guard = self.refresh_lock_for(id);
            let _guard = _refresh_guard.lock().await;
            let proxy = self.effective_proxy_for_cred(&credentials)?;
            let new_creds =
                refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await?;
            {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    entry.credentials = new_creds.clone();
                    entry.refresh_token_hash = credential_secret_hash(&new_creds);
                }
            }
            let _ = self.persist_credentials();
            new_creds
                .access_token
                .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
        } else {
            credentials
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
        };

        let proxy = self.effective_proxy_for_cred(&credentials)?;

        // IdC 账号开启超额需要 profileArn，但 IdC token 刷新时未必返回。复用 ensure_idc_profile_arn
        // 跨区解析（解析失败时用兜底 ARN 但不持久化，避免瞬时失败永久钉死错误 ARN）。
        let credentials = self
            .ensure_idc_profile_arn(id, credentials, &config, &token)
            .await;

        match set_user_preference(&credentials, &config, &token, &normalized, proxy.as_ref()).await
        {
            Ok(()) => {
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials.last_overage_status = Some(normalized.clone());
                    }
                }
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("overage_status 更新后持久化失败（不影响本次请求）: {}", e);
                }
                Ok(normalized)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("reached the limit for overages") {
                    tracing::warn!(
                        credential_id = id,
                        "凭据命中 overages 限额，自动禁用并标记 InsufficientBalance"
                    );
                    self.record_disable_event(
                        id,
                        DisableReason::InsufficientBalance,
                        None,
                        format!("overages 限额: {}", msg),
                        None,
                        None,
                        None,
                        None,
                        None,
                    );
                    self.mark_insufficient_balance(id);
                }
                Err(e)
            }
        }
    }

    /// 强制刷新指定凭据的 Token（Admin API）
    pub async fn force_refresh_token_for(&self, id: u64) -> anyhow::Result<()> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 持有刷新锁（每凭据一把），避免与业务请求自动刷新并发
        let _refresh_guard = self.refresh_lock_for(id);
        let _guard = _refresh_guard.lock().await;

        if credentials.is_api_key_credential() {
            anyhow::bail!("API Key 凭据无需刷新 Token");
        }

        let proxy = self.effective_proxy_for_cred(&credentials)?;
        let new_creds = refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await?;

        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.credentials = new_creds.clone();
                entry.refresh_failure_count = 0;
                entry.refresh_token_hash = credential_secret_hash(&new_creds);

                // 仅对自动禁用（失败阈值/刷新失败）自动恢复，手动禁用状态保持不变
                if entry.disabled && entry.disable_reason != Some(DisableReason::Manual) {
                    entry.failure_count = 0;
                    entry.disabled = false;
                    entry.auto_heal_reason = None;
                    entry.disable_reason = None;
                }
            }
        }

        self.persist_credentials()?;
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 检查是否需要刷新 token
        let needs_refresh = is_token_expired(&credentials) || is_token_expiring_soon(&credentials);

        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 kiroApiKey"))?
        } else if needs_refresh {
            let _refresh_guard = self.refresh_lock_for(id);
            let _guard = _refresh_guard.lock().await;
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
            };

            if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                let proxy = self.effective_proxy_for_cred(&current_creds)?;
                let new_creds = match refresh_token_with_id(
                    &current_creds,
                    &config,
                    proxy.as_ref(),
                    id,
                )
                .await
                {
                    Ok(creds) => creds,
                    Err(err) => {
                        if is_invalid_grant_error(&err) {
                            self.record_disable_event(
                                id,
                                DisableReason::AuthenticationFailed,
                                None,
                                format!("余额查询前 Token 刷新失败（invalid_grant）: {}", err),
                                None,
                                None,
                                None,
                                None,
                                None,
                            );
                            self.mark_authentication_failed(id);
                        }
                        return Err(err);
                    }
                };
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                        // 更新哈希缓存
                        entry.refresh_token_hash = credential_secret_hash(&new_creds);
                    }
                }
                // 持久化失败只记录警告，不影响本次请求
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                }
                new_creds
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
            } else {
                current_creds
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
            }
        } else {
            credentials
                .access_token
                .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
        };

        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // Enterprise/Q Developer 账号的 getUsageLimits 也必须带正确 region + profileArn，否则 403。
        // 与数据面一致，在此按需跨区解析并写回 profileArn（解析后 effective_api_region 会从 ARN
        // 得到正确的数据面 region）。
        let config = self.config.read().clone();
        let credentials = self
            .ensure_idc_profile_arn(id, credentials, &config, &token)
            .await;

        let proxy = self.effective_proxy_for_cred(&credentials)?;
        match get_usage_limits(&credentials, &config, &token, proxy.as_ref()).await {
            Ok(usage) => {
                let mut should_persist = false;
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        // 同步订阅等级
                        if let Some(subscription_title) = usage.subscription_title()
                            && entry.credentials.subscription_title.as_deref()
                                != Some(subscription_title)
                        {
                            entry.credentials.subscription_title =
                                Some(subscription_title.to_string());
                            should_persist = true;
                        }
                        // 同步 overage_status（落库，重启不丢）
                        if let Some(s) = usage.overage_status() {
                            let normalized = s.trim().to_ascii_uppercase();
                            if !normalized.is_empty()
                                && entry.credentials.last_overage_status.as_deref()
                                    != Some(normalized.as_str())
                            {
                                entry.credentials.last_overage_status = Some(normalized);
                                should_persist = true;
                            }
                        }
                        // 自动回填邮箱：当上游返回 email 且本地为空时持久化
                        if entry
                            .credentials
                            .email
                            .as_deref()
                            .map(str::trim)
                            .is_none_or(str::is_empty)
                            && let Some(email) = usage.extract_email()
                        {
                            entry.credentials.email = Some(email);
                            should_persist = true;
                        }
                    }
                }

                if should_persist && let Err(e) = self.persist_credentials() {
                    tracing::warn!("订阅等级更新后持久化失败（不影响本次请求）: {}", e);
                }

                Ok(usage)
            }
            Err(err) => {
                if is_invalid_grant_error(&err) {
                    self.record_disable_event(
                        id,
                        DisableReason::AuthenticationFailed,
                        None,
                        format!("getUsageLimits 失败（invalid_grant）: {}", err),
                        None,
                        None,
                        None,
                        None,
                        None,
                    );
                    self.mark_authentication_failed(id);
                } else if is_temporarily_suspended_error(&err) {
                    self.record_disable_event(
                        id,
                        DisableReason::AccountSuspended,
                        None,
                        format!("getUsageLimits 失败（TEMPORARILY_SUSPENDED）: {}", err),
                        None,
                        None,
                        None,
                        None,
                        None,
                    );
                    self.mark_account_suspended(id);
                }
                Err(err)
            }
        }
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（refresh_token 不为空）
    /// 2. 基于 refreshToken 的 SHA-256 哈希检测重复
    /// 3. 尝试刷新 Token 验证凭据有效性
    /// 4. 分配新 ID（当前最大 ID + 1）
    /// 5. 添加到 entries 列表
    /// 6. 持久化到配置文件
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, new_cred: KiroCredentials) -> anyhow::Result<u64> {
        self.add_credential_inner(new_cred, true).await
    }

    /// 添加新凭据但跳过 refresh / usage 验证（Admin API）
    ///
    /// 用于「暂不绑定代理」场景：尚无可用代理槽时强制走上游验证只会失败，
    /// 改为直接落库，由调用方负责把凭据置为禁用，待手动绑定代理后再激活。
    pub async fn add_credential_unverified(
        &self,
        new_cred: KiroCredentials,
    ) -> anyhow::Result<u64> {
        self.add_credential_inner(new_cred, false).await
    }

    async fn add_credential_inner(
        &self,
        new_cred: KiroCredentials,
        verify: bool,
    ) -> anyhow::Result<u64> {
        let config = self.config.read().clone();
        // 1. 基本验证
        validate_credential_secret(&new_cred)?;

        // 2. 基于 refreshToken / API Key 的 SHA-256 哈希检测重复
        let new_secret_hash = credential_secret_hash(&new_cred)
            .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken 或 kiroApiKey"))?;
        let duplicate_exists = {
            let entries = self.entries.lock();
            entries.iter().any(|entry| {
                let hash = entry
                    .refresh_token_hash
                    .clone()
                    .or_else(|| credential_secret_hash(&entry.credentials));
                hash.as_deref() == Some(new_secret_hash.as_str())
            })
        };
        if duplicate_exists {
            anyhow::bail!("凭据已存在（refreshToken 或 kiroApiKey 重复）");
        }

        // 3. 尝试验证凭据有效性（verify=false 时跳过，避免没绑代理就触发上游调用）
        let mut validated_cred = if !verify {
            let mut cred = new_cred.clone();
            cred.access_token = None;
            cred.expires_at = None;
            cred.subscription_title = None;
            cred
        } else {
            let proxy = self.effective_proxy_for_cred(&new_cred)?;
            if new_cred.is_api_key_credential() {
                let token = new_cred
                    .kiro_api_key
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;
                let usage = get_usage_limits(&new_cred, &config, &token, proxy.as_ref()).await?;
                let mut cred = new_cred.clone();
                cred.access_token = None;
                cred.expires_at = None;
                cred.subscription_title = usage.subscription_title().map(|s| s.to_string());
                cred
            } else {
                refresh_token(&new_cred, &config, proxy.as_ref()).await?
            }
        };

        // 4-6. 在同一把锁内「分配 ID → 构建 entry → push」。
        // 到这里所有 .await（token 刷新/余额查询）都已完成，下面全是同步计算，
        // 因此可以整段持锁：避免两个并发 add 在锁外各自算出相同的 max+1 得到重复 ID
        // —— 重复 ID 会让后续 replace_all_credentials 因主键冲突整事务回滚，导致持久化永久失败。
        let new_id = {
            let mut entries = self.entries.lock();
            let new_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;

            // 5. 设置 ID 并保留用户输入的元数据
            validated_cred.id = Some(new_id);
            validated_cred.priority = new_cred.priority;
            validated_cred.auth_method = new_cred.auth_method.clone();
            validated_cred.canonicalize_auth_method();
            validated_cred.client_id = new_cred.client_id;
            validated_cred.client_secret = new_cred.client_secret;
            validated_cred.region = new_cred.region;
            validated_cred.machine_id = new_cred.machine_id;
            validated_cred.email = new_cred.email;
            validated_cred.api_region = new_cred.api_region;
            validated_cred.proxy_slot_id = new_cred.proxy_slot_id;
            validated_cred.kiro_api_key = new_cred.kiro_api_key;
            validated_cred.allow_overuse = new_cred.allow_overuse;
            validated_cred.rpm = new_cred.rpm.filter(|&v| v > 0);

            let entry_secret_hash = credential_secret_hash(&validated_cred);

            entries.push(CredentialEntry {
                id: new_id,
                credentials: validated_cred,
                failure_count: 0,
                refresh_failure_count: 0,
                disabled: false,
                auto_heal_reason: None,
                disable_reason: None,
                success_count: 0,
                error_count: 0,
                last_used_at: None,
                last_acquired_at: None,
                refresh_token_hash: entry_secret_hash,
                in_flight: Arc::new(AtomicU32::new(0)),
                rpm: RpmTracker::new(),
                rate_limit_count: Arc::new(AtomicU32::new(0)),
                model_usage: HashMap::new(),
                recent_outcomes: VecDeque::new(),
            });

            new_id
        };

        // 同步凭据级 RPM 到 rate_limiter（避免重启或重复添加时漏配）
        if let Some(rpm) = new_cred.rpm.filter(|&v| v > 0) {
            let interval_ms = (60_000u64 / rpm as u64).max(1);
            self.rate_limiter
                .set_credential_min_interval(new_id, Some(interval_ms));
        }

        // 6. 持久化
        self.persist_credentials()?;

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 删除凭据（Admin API）
    ///
    /// # 前置条件
    /// - 凭据必须已禁用（disabled = true）
    ///
    /// # 行为
    /// 1. 验证凭据存在
    /// 2. 验证凭据已禁用
    /// 3. 从 entries 移除
    /// 4. 持久化到文件
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();

            // 查找凭据
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            // 检查是否已禁用
            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }

            // 删除凭据
            entries.retain(|e| e.id != id);
        }

        // 清理凭据级速率覆盖（避免新凭据复用同 ID 时残留）
        self.rate_limiter.set_credential_min_interval(id, None);
        self.rate_limiter.reset(id);

        // 持久化更改
        self.persist_credentials()?;

        // 立即回写统计数据，清除已删除凭据的残留条目
        self.save_stats();

        tracing::info!("已删除凭据 #{}", id);
        Ok(())
    }

    /// 检查是否存在具有相同 refreshToken 前缀的凭据
    ///
    /// 用于批量导入时的去重检查，通过比较 refreshToken 前 32 字符判断是否重复
    /// 使用 floor_char_boundary 安全截断，避免在多字节字符中间切割导致 panic
    /// 检查是否已存在指定的 Kiro API Key（精确匹配，用于批量导入去重）
    pub fn has_kiro_api_key(&self, kiro_api_key: &str) -> bool {
        let target = kiro_api_key.trim();
        let entries = self.entries.lock();
        entries.iter().any(|e| {
            e.credentials
                .kiro_api_key
                .as_deref()
                .is_some_and(|k| k.trim() == target)
        })
    }

    pub fn has_refresh_token_prefix(&self, refresh_token: &str) -> bool {
        let prefix_len = floor_char_boundary(refresh_token, 32);
        let new_prefix = &refresh_token[..prefix_len];

        let entries = self.entries.lock();
        entries.iter().any(|e| {
            e.credentials
                .refresh_token
                .as_ref()
                .map(|rt| {
                    let existing_prefix_len = floor_char_boundary(rt, 32);
                    &rt[..existing_prefix_len] == new_prefix
                })
                .unwrap_or(false)
        })
    }

    // ========================================================================
    // 增强特性：速率限制、冷却管理、后台刷新
    // ========================================================================

    #[allow(dead_code)]
    /// 获取速率限制器引用
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// 获取冷却管理器引用
    #[allow(dead_code)]
    pub fn cooldown_manager(&self) -> &CooldownManager {
        &self.cooldown_manager
    }

    /// 检查凭据是否可用（综合检查：未禁用、未冷却、未超速率限制）
    #[allow(dead_code)]
    pub fn is_credential_available(&self, id: u64) -> bool {
        // 检查是否禁用
        let is_disabled = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.disabled)
                .unwrap_or(true)
        };
        if is_disabled {
            return false;
        }

        // 检查冷却状态
        if !self.cooldown_manager.is_available(id) {
            return false;
        }

        // 检查速率限制
        self.rate_limiter.check_rate_limit(id).is_ok()
    }

    /// 设置凭据冷却（带原因分类）
    #[allow(dead_code)]
    pub fn set_credential_cooldown(&self, id: u64, reason: CooldownReason) -> std::time::Duration {
        self.cooldown_manager.set_cooldown(id, reason)
    }

    /// 设置凭据冷却（支持自定义时长）
    #[allow(dead_code)]
    pub fn set_credential_cooldown_with_duration(
        &self,
        id: u64,
        reason: CooldownReason,
        duration: Option<std::time::Duration>,
    ) -> std::time::Duration {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
            }
        }
        self.save_stats_debounced();
        self.cooldown_manager
            .set_cooldown_with_duration(id, reason, duration)
    }

    /// 清除凭据冷却
    #[allow(dead_code)]
    pub fn clear_credential_cooldown(&self, id: u64) -> bool {
        self.cooldown_manager.clear_cooldown(id)
    }

    /// 获取即将过期的凭据 ID 列表
    ///
    /// # Arguments
    /// * `minutes_before_expiry` - 过期前多少分钟视为即将过期
    #[allow(dead_code)]
    pub fn get_expiring_credential_ids(&self, minutes_before_expiry: i64) -> Vec<u64> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| {
                !e.disabled
                    && is_token_expiring_within(&e.credentials, minutes_before_expiry)
                        .unwrap_or(false)
            })
            .map(|e| e.id)
            .collect()
    }

    /// 启动后台 Token 刷新任务
    ///
    /// 定期检查并预刷新即将过期的 Token，避免请求时的刷新延迟。
    /// 返回 `BackgroundRefresher` 的 `Arc` 引用，调用方需要保持该引用以维持后台任务运行。
    #[allow(dead_code)]
    pub fn start_background_refresh(
        self: &Arc<Self>,
        config: BackgroundRefreshConfig,
    ) -> Arc<BackgroundRefresher> {
        let refresher = Arc::new(BackgroundRefresher::new(config.clone()));
        let manager = Arc::clone(self);
        let manager_for_ids = Arc::clone(self);

        let refresh_before_mins = config.refresh_before_expiry_mins;

        if let Err(e) = refresher.start(
            move |id| {
                let manager = Arc::clone(&manager);
                Box::pin(async move {
                    match manager.refresh_token_for_credential(id).await {
                        Ok(_) => {
                            tracing::debug!("后台刷新凭据 #{} Token 成功", id);
                            true
                        }
                        Err(e) => {
                            tracing::warn!("后台刷新凭据 #{} Token 失败: {}", id, e);
                            false
                        }
                    }
                })
            },
            move |mins| manager_for_ids.get_expiring_credential_ids(mins.max(refresh_before_mins)),
        ) {
            tracing::error!("启动后台刷新任务失败: {}", e);
        }

        tracing::info!("后台 Token 刷新任务已启动");
        refresher
    }

    /// 刷新指定凭据的 Token（带优雅降级）
    ///
    /// 如果刷新失败但现有 Token 仍有效，返回现有 Token（优雅降级）
    #[allow(dead_code)]
    pub async fn refresh_token_for_credential(&self, id: u64) -> anyhow::Result<RefreshResult> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 尝试刷新
        let proxy = self.effective_proxy_for_cred(&credentials)?;
        match refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await {
            Ok(new_creds) => {
                // 更新凭据
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                        // 更新哈希缓存
                        entry.refresh_token_hash =
                            new_creds.refresh_token.as_deref().map(sha256_hex);
                    }
                }

                // 持久化
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败: {}", e);
                }

                let expires_at = new_creds.expires_at.unwrap_or_default();
                Ok(RefreshResult::success(id, expires_at))
            }
            Err(e) => {
                // 优雅降级：检查现有 Token 是否仍有效
                if !is_token_expired(&credentials) {
                    let expires_at = credentials.expires_at.unwrap_or_default();
                    tracing::warn!(
                        "凭据 #{} Token 刷新失败，使用现有 Token（优雅降级）: {}",
                        id,
                        e
                    );
                    Ok(RefreshResult::fallback(id, expires_at))
                } else {
                    // 设置冷却
                    self.cooldown_manager
                        .set_cooldown(id, CooldownReason::TokenRefreshFailed);
                    Err(e)
                }
            }
        }
    }

    /// 记录 API 调用成功（更新速率限制器）
    #[allow(dead_code)]
    pub fn record_api_success(&self, id: u64) {
        self.report_success(id);
        self.rate_limiter.record_success(id);
    }

    /// 记录 API 调用失败（更新速率限制器和冷却管理器）
    #[allow(dead_code)]
    pub fn record_api_failure(&self, id: u64, error_message: Option<&str>) -> bool {
        let has_available = self.report_failure(id);

        // 更新速率限制器
        let backoff = self.rate_limiter.record_failure(id, error_message);
        tracing::debug!("凭据 #{} 退避时间: {:?}", id, backoff);

        has_available
    }

    /// 清理过期的冷却状态
    #[allow(dead_code)]
    pub fn cleanup_expired_cooldowns(&self) -> usize {
        self.cooldown_manager.cleanup_expired()
    }
}

impl Drop for MultiTokenManager {
    fn drop(&mut self) {
        if self.stats_dirty.load(Ordering::Relaxed) {
            self.save_stats();
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_token_manager_new() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let tm = TokenManager::new(config, credentials, None);
        assert!(tm.credentials().access_token.is_none());
    }

    #[test]
    fn test_structured_suspend_pattern_matches_only_error_fields() {
        // 结构化错误字段命中封号关键字
        assert_eq!(
            structured_suspend_pattern(r#"{"reason":"ACCOUNT_SUSPENDED"}"#),
            Some("ACCOUNT_SUSPENDED")
        );
        assert_eq!(
            structured_suspend_pattern(r#"{"__type":"AccountSuspendedException","message":"x"}"#),
            Some("AccountSuspendedException")
        );
        assert_eq!(
            structured_suspend_pattern(r#"{"error":{"reason":"TEMPORARILY_SUSPENDED"}}"#),
            Some("TEMPORARILY_SUSPENDED")
        );
        // 用户 prompt 里出现同名字符串（在 conversationState 等非错误字段）不应误判为封号
        assert_eq!(
            structured_suspend_pattern(
                r#"{"conversationState":{"content":"explain AccountSuspendedException"},"reason":"THROTTLING"}"#
            ),
            None
        );
        // 普通 429/瞬态错误不误判
        assert_eq!(
            structured_suspend_pattern(r#"{"message":"Too many requests","reason":"THROTTLING"}"#),
            None
        );
        // 非 JSON
        assert_eq!(structured_suspend_pattern("Internal Server Error"), None);
    }

    #[test]
    fn test_mask_user_id_multibyte_no_panic() {
        // 回归：user_id 来自客户端，含多字节字符时曾用 &id[..4]/&id[len-4..] 裸切片 panic。
        assert_eq!(mask_user_id(None), "None");
        assert_eq!(mask_user_id(Some("short")), "***"); // len <= 12
        // 首尾均为多字节，byte 4 / len-4 都可能落在字符中间；只要不 panic 且做了掩码即可
        let masked = mask_user_id(Some("你好世界你好世界你好世界"));
        assert!(masked.contains("***"));
        // ASCII 长串保持原行为：前 4 + *** + 后 4
        assert_eq!(mask_user_id(Some("abcdefghijklmnop")), "abcd***mnop");
    }

    #[test]
    fn test_replace_error_message_field_top_level_message() {
        let body = r#"{"type":"error","code":429,"message":"Too many requests"}"#;
        let out = replace_error_message_field(body, "请稍后重试").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["message"], "请稍后重试");
        assert_eq!(v["type"], "error"); // 外层结构保留
        assert_eq!(v["code"], 429);
    }

    #[test]
    fn test_replace_error_message_field_nested_anthropic_style() {
        let body =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limit hit"}}"#;
        let out = replace_error_message_field(body, "已限流").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["message"], "已限流");
        assert_eq!(v["error"]["type"], "rate_limit_error"); // 同级 type 不动
        assert_eq!(v["type"], "error"); // 顶层 type 不动
    }

    #[test]
    fn test_replace_error_message_field_returns_none_for_non_json() {
        assert!(replace_error_message_field("not a json body", "x").is_none());
    }

    #[test]
    fn test_replace_error_message_field_returns_none_when_no_known_field() {
        // JSON 但无 message / error.message 等已知字段 → 调用方应回退整段替换
        let body = r#"{"type":"error","code":500,"foo":"bar"}"#;
        assert!(replace_error_message_field(body, "x").is_none());
    }

    #[test]
    fn test_match_error_replacement_keeps_json_shell() {
        let mut config = Config::default();
        config.error_replace_rules = vec!["MONTHLY_REQUEST_COUNT===请稍后重试".to_string()];
        let mut cred = KiroCredentials::default();
        cred.refresh_token = Some("dummy".repeat(40));
        let mgr = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let body = r#"{"error":{"type":"throttling","message":"MONTHLY_REQUEST_COUNT exceeded"}}"#;
        let out = mgr.match_error_replacement(body).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["message"], "请稍后重试");
        assert_eq!(v["error"]["type"], "throttling"); // 关键：原结构保留
    }

    #[test]
    fn test_match_error_replacement_falls_back_to_whole_body_replace() {
        let mut config = Config::default();
        config.error_replace_rules = vec!["bad===unauthorized".to_string()];
        let mut cred = KiroCredentials::default();
        cred.refresh_token = Some("dummy".repeat(40));
        let mgr = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        // 非 JSON：命中即整段替换（旧行为）
        let out = mgr.match_error_replacement("token is bad").unwrap();
        assert_eq!(out, "unauthorized");
    }

    fn make_pool_entry(
        id: &str,
        slots: u32,
        hours_valid: i64,
    ) -> crate::kiro::proxy_pool::ProxyEntry {
        crate::kiro::proxy_pool::ProxyEntry {
            id: id.to_string(),
            url: format!("http://{}.example.com:1080", id),
            username: None,
            password: None,
            expires_at: chrono::Utc::now() + chrono::Duration::hours(hours_valid),
            slots,
            bound_credential_ids: vec![],
            label: None,
            created_at: chrono::Utc::now(),
            last_rotated_at: None,
            disabled: false,
            disabled_category: None,
            disabled_reason: None,
        }
    }

    #[test]
    fn test_proxy_network_failure_threshold_switches_to_available_proxy() {
        let mut config = Config::default();
        config.proxy_failure_threshold = 2;
        let mut cred = KiroCredentials::default();
        cred.id = Some(1);
        cred.refresh_token = Some("dummy".repeat(40));
        cred.proxy_slot_id = Some("p-bad".to_string());
        let mgr = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        let pool = crate::kiro::proxy_pool::ProxyPool::empty();
        pool.add_many_force(vec![
            make_pool_entry("p-bad", 1, 100),
            make_pool_entry("p-good", 1, 100),
        ]);
        pool.manual_bind("p-bad", 1).unwrap();
        mgr.set_proxy_pool(pool.clone());

        // 第 1 次失败：未达阈值，无动作
        mgr.report_proxy_network_failure(1);
        assert!(!pool.get("p-bad").unwrap().disabled);

        // 第 2 次失败：达到阈值 → p-bad 被标记不可用，凭据换绑到 p-good
        mgr.report_proxy_network_failure(1);
        let bad = pool.get("p-bad").unwrap();
        assert!(bad.disabled);
        assert_eq!(
            bad.disabled_category,
            Some(crate::kiro::proxy_pool::ProxyDisabledCategory::NetworkFailure)
        );
        assert!(bad.bound_credential_ids.is_empty());
        assert!(
            pool.get("p-good")
                .unwrap()
                .bound_credential_ids
                .contains(&1)
        );

        let snap = mgr.snapshot();
        let entry = snap.entries.iter().find(|e| e.id == 1).unwrap();
        assert_eq!(entry.proxy_slot_id.as_deref(), Some("p-good"));
        assert!(!entry.disabled);

        // 成功响应后计数归零（换绑后再失败 1 次不触发新一轮禁用）
        mgr.report_proxy_network_success(1);
        mgr.report_proxy_network_failure(1);
        assert!(!pool.get("p-good").unwrap().disabled);
    }

    #[test]
    fn test_proxy_network_failure_no_candidate_disables_credential() {
        let mut config = Config::default();
        config.proxy_failure_threshold = 2;
        let mut cred = KiroCredentials::default();
        cred.id = Some(1);
        cred.refresh_token = Some("dummy".repeat(40));
        cred.proxy_slot_id = Some("p-only".to_string());
        let mgr = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 池中只有这一个代理，无可换绑候选
        let pool = crate::kiro::proxy_pool::ProxyPool::empty();
        pool.add_many_force(vec![make_pool_entry("p-only", 1, 100)]);
        pool.manual_bind("p-only", 1).unwrap();
        mgr.set_proxy_pool(pool.clone());

        mgr.report_proxy_network_failure(1);
        mgr.report_proxy_network_failure(1);

        // 代理被标记不可用；凭据解绑并被禁用（无可用代理）
        assert!(pool.get("p-only").unwrap().disabled);
        assert!(pool.get("p-only").unwrap().bound_credential_ids.is_empty());
        let snap = mgr.snapshot();
        let entry = snap.entries.iter().find(|e| e.id == 1).unwrap();
        assert_eq!(entry.proxy_slot_id, None);
        assert!(entry.disabled);
        assert_eq!(entry.disable_reason, Some(DisableReason::ProxyUnavailable));
    }

    #[test]
    fn test_proxy_network_failure_threshold_zero_disables_feature() {
        let mut config = Config::default();
        config.proxy_failure_threshold = 0;
        let mut cred = KiroCredentials::default();
        cred.id = Some(1);
        cred.refresh_token = Some("dummy".repeat(40));
        cred.proxy_slot_id = Some("p-x".to_string());
        let mgr = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        let pool = crate::kiro::proxy_pool::ProxyPool::empty();
        pool.add_many_force(vec![make_pool_entry("p-x", 1, 100)]);
        pool.manual_bind("p-x", 1).unwrap();
        mgr.set_proxy_pool(pool.clone());

        for _ in 0..10 {
            mgr.report_proxy_network_failure(1);
        }
        assert!(!pool.get("p-x").unwrap().disabled);
    }

    #[test]
    fn test_is_token_expired_with_expired_token() {
        let mut credentials = KiroCredentials::default();
        credentials.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_with_valid_token() {
        let mut credentials = KiroCredentials::default();
        let future = Utc::now() + Duration::hours(1);
        credentials.expires_at = Some(future.to_rfc3339());
        assert!(!is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_within_5_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(3);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_no_expires_at() {
        let credentials = KiroCredentials::default();
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_within_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(8);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_beyond_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(15);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(!is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_validate_refresh_token_missing() {
        let credentials = KiroCredentials::default();
        let result = validate_refresh_token(&credentials);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_refresh_token_valid() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        let result = validate_refresh_token(&credentials);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_refresh_token() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.refresh_token = Some("a".repeat(150));

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("凭据已存在"));
    }

    // MultiTokenManager 测试

    #[test]
    fn test_multi_token_manager_new() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.priority = 0;
        let mut cred2 = KiroCredentials::default();
        cred2.priority = 1;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    #[test]
    fn test_multi_token_manager_api_key_missing_kiro_api_key_auto_disabled() {
        let config = Config::default();

        // auth_method=api_key 但缺少 kiro_api_key → 启动即自动禁用
        let mut bad_cred = KiroCredentials::default();
        bad_cred.auth_method = Some("api_key".to_string());

        let mut good_cred = KiroCredentials::default();
        good_cred.refresh_token = Some("a".repeat(150));

        let manager =
            MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_multi_token_manager_api_key_empty_kiro_api_key_auto_disabled() {
        let config = Config::default();

        // kiro_api_key 为空白 → 同样视为无效配置
        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        cred.kiro_api_key = Some("   ".to_string());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_api_key_with_kiro_api_key_not_disabled() {
        let config = Config::default();

        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        cred.kiro_api_key = Some("ksk_test123".to_string());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_has_kiro_api_key() {
        let config = Config::default();
        let mut cred = KiroCredentials::default();
        cred.kiro_api_key = Some("ksk_existing_key".to_string());
        cred.auth_method = Some("api_key".to_string());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        assert!(manager.has_kiro_api_key("ksk_existing_key"));
        assert!(manager.has_kiro_api_key("  ksk_existing_key  ")); // trim 后匹配
        assert!(!manager.has_kiro_api_key("ksk_other_key"));
    }

    #[test]
    fn test_reset_and_enable_rejects_invalid_config_credential() {
        let config = Config::default();

        let mut bad_cred = KiroCredentials::default();
        bad_cred.auth_method = Some("api_key".to_string());

        let manager = MultiTokenManager::new(config, vec![bad_cred], None, None, false).unwrap();
        let snapshot = manager.snapshot();
        let id = snapshot.entries[0].id;

        let err = manager.reset_and_enable(id).unwrap_err();
        assert!(err.to_string().contains("配置无效"));
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_invalidate_access_token_marks_expired() {
        let config = Config::default();
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        credentials.access_token = Some("some_token".to_string());
        credentials.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![credentials], None, None, false).unwrap();
        assert!(manager.invalidate_access_token(1));

        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        let mut cred = KiroCredentials::default();
        cred.expires_at = entry.expires_at.clone();
        assert!(is_token_expired(&cred));
    }

    #[test]
    fn test_multi_token_manager_empty_credentials() {
        let config = Config::default();
        let result = MultiTokenManager::new(config, vec![], None, None, false);
        // 支持 0 个凭据启动（可通过管理面板添加）
        assert!(result.is_ok());
        let manager = result.unwrap();
        assert_eq!(manager.total_count(), 0);
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_duplicate_ids() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.id = Some(1);
        let mut cred2 = KiroCredentials::default();
        cred2.id = Some(1); // 重复 ID

        let result = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false);
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("重复的凭据 ID"),
            "错误消息应包含 '重复的凭据 ID'，实际: {}",
            err_msg
        );
    }

    #[test]
    fn test_multi_token_manager_report_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        // MAX_FAILURES_PER_CREDENTIAL = 3，所以前两次失败不会禁用
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);

        // 第三次失败会禁用第一个凭据
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 1);

        // 继续失败第二个凭据（使用 ID 2），需要 3 次才会禁用
        assert!(manager.report_failure(2));
        assert!(manager.report_failure(2));
        assert!(!manager.report_failure(2)); // 所有凭据都禁用了
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_report_success() {
        let config = Config::default();
        let cred = KiroCredentials::default();

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 失败一次（使用 ID 1）
        manager.report_failure(1);

        // 成功后重置计数（使用 ID 1）
        manager.report_success(1);

        // 再失败一次不会禁用（因为计数已重置）
        manager.report_failure(1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_record_upstream_error_increments_error_count_only() {
        let config = Config::default();
        let cred = KiroCredentials::default();

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 连续调用 MAX_FAILURES_PER_CREDENTIAL 次也不应触发禁用（不动 failure_count）
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.record_upstream_error(1);
        }
        assert_eq!(manager.available_count(), 1);

        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        assert_eq!(entry.error_count, MAX_FAILURES_PER_CREDENTIAL as u64);
        assert_eq!(entry.failure_count, 0);
    }

    #[test]
    fn test_stats_entry_deserializes_without_rate_limit_count() {
        // 旧统计缓存无 rate_limit_count / recent_outcomes 字段时按默认值处理
        let json = r#"{"success_count": 5, "error_count": 2, "last_used_at": null}"#;
        let entry: StatsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.success_count, 5);
        assert_eq!(entry.error_count, 2);
        assert_eq!(entry.rate_limit_count, 0);
        assert!(entry.recent_outcomes.is_empty());
    }

    #[test]
    fn test_recent_outcomes_ring_and_roundtrip() {
        // 环形缓冲：超容量淘汰最旧的
        let mut dq = VecDeque::new();
        for _ in 0..RECENT_OUTCOMES_CAP {
            push_recent_outcome(&mut dq, OUTCOME_SUCCESS);
        }
        push_recent_outcome(&mut dq, OUTCOME_RATE_LIMIT);
        assert_eq!(dq.len(), RECENT_OUTCOMES_CAP);
        assert_eq!(*dq.back().unwrap(), OUTCOME_RATE_LIMIT);

        // 编码保持顺序（旧 → 新），三种结果字符正确
        let mut dq = VecDeque::new();
        push_recent_outcome(&mut dq, OUTCOME_ERROR);
        push_recent_outcome(&mut dq, OUTCOME_SUCCESS);
        push_recent_outcome(&mut dq, OUTCOME_RATE_LIMIT);
        assert_eq!(encode_recent_outcomes(&dq), "esr");

        // 编解码 roundtrip
        let mut dq = VecDeque::new();
        for i in 0..250 {
            let o = match i % 10 {
                0 => OUTCOME_ERROR,
                1 => OUTCOME_RATE_LIMIT,
                _ => OUTCOME_SUCCESS,
            };
            push_recent_outcome(&mut dq, o);
        }
        let decoded = decode_recent_outcomes(&encode_recent_outcomes(&dq));
        assert_eq!(decoded, dq);

        // 超长字符串只取最新 CAP 条
        let long = "e".repeat(50) + &"s".repeat(RECENT_OUTCOMES_CAP);
        let decoded = decode_recent_outcomes(&long);
        assert_eq!(decoded.len(), RECENT_OUTCOMES_CAP);
        assert!(decoded.iter().all(|&o| o == OUTCOME_SUCCESS));
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(1);
        }
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(2);
        }

        assert_eq!(manager.available_count(), 0);

        // 应触发自愈：重置失败计数并重新启用，避免必须重启进程
        let ctx = manager.acquire_context().await.unwrap();
        assert!(ctx.token == "t1" || ctx.token == "t2");
        assert_eq!(manager.available_count(), 2);
    }

    /// 高并发下 LRU 仍能均匀分配：100 个并发 acquire 跨 4 个号，
    /// 每个号被选中次数都应该接近 100/4=25。
    ///
    /// 这是对"持锁选号+立即更新时间戳"修复的回归测试——
    /// 如果改回"锁外 select + 锁内更新"的旧结构，并发请求会拿到过期快照，
    /// 大量请求会瞬时偏向同一个最旧号，分布严重不均。
    ///
    /// 用 multi_thread runtime 确保 spawn 出的 100 个 task 真正在不同 OS 线程上
    /// 并发抢锁，而不是 current_thread 下的协作式调度（那种环境下争用极少，
    /// 测试可能给出错误的安全感）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_multi_token_manager_acquire_context_lru_distributes_under_concurrency() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let config = Config::default();
        let mut creds = Vec::new();
        for i in 1..=4 {
            let mut c = KiroCredentials::default();
            c.access_token = Some(format!("t{}", i));
            c.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            creds.push(c);
        }

        let manager = Arc::new(MultiTokenManager::new(config, creds, None, None, false).unwrap());

        // 4 个号的命中计数（id 从 1 开始）
        let counters: Arc<[AtomicU32; 4]> = Arc::new([
            AtomicU32::new(0),
            AtomicU32::new(0),
            AtomicU32::new(0),
            AtomicU32::new(0),
        ]);

        let total = 100usize;
        let mut handles = Vec::with_capacity(total);
        for _ in 0..total {
            let m = Arc::clone(&manager);
            let c = Arc::clone(&counters);
            handles.push(tokio::spawn(async move {
                let ctx = m.acquire_context().await.unwrap();
                c[(ctx.id - 1) as usize].fetch_add(1, Ordering::Relaxed);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let counts: Vec<u32> = (0..4)
            .map(|i| counters[i].load(Ordering::Relaxed))
            .collect();
        let min = *counts.iter().min().unwrap();
        let max = *counts.iter().max().unwrap();
        let sum: u32 = counts.iter().sum();

        assert_eq!(sum as usize, total, "全部请求都应记入计数：{:?}", counts);
        // 完美均匀时 25/25/25/25；允许 ±5 容差，足以容纳调度抖动但能 catch
        // "偏向单号"的回归（典型偏向场景下 max-min 会 >= 30）
        assert!(
            max - min <= 5,
            "并发分配应均匀（max={}, min={}, counts={:?}）",
            max,
            min,
            counts
        );
    }

    /// LRU 选号核心：从未被选中（None）优先；都用过时 `last_acquired_at` 最早者优先。
    ///
    /// 直接单元测试 `select_best_candidate_id`，而不是走 `acquire_context` 端到端路径：
    /// 端到端时被选中的凭据会立刻进入 rate_limiter 的最小间隔限制（其计时独立于
    /// `last_acquired_at`），两个凭据在毫秒内相继被 acquire 会同时被限流并触发重试轮转，
    /// 最终返回哪个由 rate_limiter 计时决定——本质上非确定，此前正是这里导致约 1/5 概率 flaky。
    #[test]
    fn test_select_best_candidate_id_prefers_least_recently_acquired() {
        let manager = MultiTokenManager::new(
            Config::default(),
            vec![KiroCredentials::default()],
            None,
            None,
            false,
        )
        .unwrap();

        let now = std::time::Instant::now();
        let older = now - std::time::Duration::from_secs(10);
        let newer = now;

        // 从未用过（None）优先于任何已用过的（Some），与顺序无关
        assert_eq!(
            manager.select_best_candidate_id(&[(1, Some(newer)), (2, None)]),
            Some(2)
        );
        assert_eq!(
            manager.select_best_candidate_id(&[(1, None), (2, Some(newer))]),
            Some(1)
        );

        // 都用过：last_acquired_at 最早（elapsed 最久）的优先，与列表顺序无关
        assert_eq!(
            manager.select_best_candidate_id(&[(1, Some(older)), (2, Some(newer))]),
            Some(1)
        );
        assert_eq!(
            manager.select_best_candidate_id(&[(1, Some(newer)), (2, Some(older))]),
            Some(2)
        );

        // 空候选返回 None
        assert_eq!(manager.select_best_candidate_id(&[]), None);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_round_robin_when_balance_and_usage_equal() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.update_balance_cache(1, 100.0);
        manager.update_balance_cache(2, 100.0);

        let ctx1 = manager.acquire_context().await.unwrap();
        let ctx2 = manager.acquire_context().await.unwrap();
        assert_ne!(ctx1.id, ctx2.id);
    }

    #[test]
    fn test_multi_token_manager_report_quota_exhausted() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_quota_exhausted(1));
        assert_eq!(manager.available_count(), 1);

        // 再禁用第二个后，无可用凭据
        assert!(!manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 0);
    }

    #[tokio::test]
    async fn test_multi_token_manager_quota_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.report_quota_exhausted(1);
        manager.report_quota_exhausted(2);
        assert_eq!(manager.available_count(), 0);

        // 刚禁用（在自动复检窗口内）时不应被恢复
        let err = manager.acquire_context().await.err().unwrap().to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
        assert_eq!(manager.available_count(), 0);
    }

    #[tokio::test]
    async fn test_multi_token_manager_quota_recovers_after_window() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();
        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.report_quota_exhausted(1);
        manager.report_quota_exhausted(2);
        // 窗口内：check_and_recover 不应恢复
        manager.check_and_recover();
        assert_eq!(manager.available_count(), 0, "复检窗口内不应恢复额度凭据");

        // 把 last_used_at（禁用时刻）拨到窗口之外，模拟额度冷却到期
        {
            let mut entries = manager.entries.lock();
            let past = (Utc::now() - Duration::hours(QUOTA_AUTO_RECOVERY_HOURS + 1)).to_rfc3339();
            for e in entries.iter_mut() {
                e.last_used_at = Some(past.clone());
            }
        }
        assert!(manager.check_and_recover(), "窗口到期后应恢复额度凭据");
        assert_eq!(
            manager.available_count(),
            2,
            "两个额度用尽的凭据窗口到期后都应被重新启用"
        );
    }

    #[tokio::test]
    async fn test_multi_token_manager_rate_limited_with_some_disabled_does_not_report_all_disabled()
    {
        // 复现线上日志：
        // - total > available（部分凭据被禁用）
        // - 所有可用凭据都被速率限制/冷却暂时挡住
        // 期望：等待最短可用时间后继续尝试，而不是误报“所有凭据均已禁用（x/y）”。

        let mut config = Config::default();
        // 固定间隔 10ms，避免测试过慢且消除抖动带来的不确定性
        config.credential_rpm = Some(6000);

        let cred1 = KiroCredentials {
            access_token: Some("token-1".to_string()),
            expires_at: Some("2999-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        let cred2 = KiroCredentials {
            access_token: Some("token-2".to_string()),
            expires_at: Some("2999-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 禁用 #2，仅保留一个可用凭据
        assert!(manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 1);

        // 预先占位：让 #1 在下一次 acquire_context() 时必然触发速率限制
        assert!(manager.rate_limiter().try_acquire(1).is_ok());

        // 关键断言：不会抛出“所有凭据均已禁用（1/2）”，而是等待后成功返回。
        let ctx = manager.acquire_context().await.unwrap();
        assert_eq!(ctx.id, 1);
    }

    #[test]
    fn test_set_credential_cooldown_with_duration_does_not_increment_failure_count() {
        let config = Config::default();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        let cooldown = manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(120)),
        );
        assert_eq!(cooldown, std::time::Duration::from_secs(120));

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].failure_count, 0);
        assert!(!snapshot.entries[0].disabled);
        assert!(snapshot.entries[0].last_used_at.is_some());

        let (reason, remaining) = manager.cooldown_manager().check_cooldown(1).unwrap();
        assert_eq!(reason, CooldownReason::RateLimitExceeded);
        assert!(remaining <= std::time::Duration::from_secs(120));
        assert!(remaining > std::time::Duration::from_secs(100));
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_skips_rate_limited_credential() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(200)),
        );

        let ctx = manager.acquire_context().await.unwrap();
        assert_eq!(ctx.id, 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_waits_until_rate_limit_cooldown_expires() {
        let config = Config::default();
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(150)),
        );

        let started = std::time::Instant::now();
        let ctx = manager.acquire_context().await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(ctx.id, 1);
        assert!(elapsed >= std::time::Duration::from_millis(120));
    }

    #[tokio::test]
    async fn test_acquire_context_bails_when_all_credentials_cooling_longer_than_threshold() {
        let config = Config::default();
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 设置 10 秒冷却，超过 2 秒阈值
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );

        let started = std::time::Instant::now();
        let err = manager.acquire_context().await.err().unwrap();
        let elapsed = started.elapsed();

        // 应立即返回错误，不会长睡
        assert!(elapsed < std::time::Duration::from_secs(2));
        assert!(err.to_string().contains("所有凭据均处于冷却/速率限制"));
        assert!(err.to_string().contains("retry_after_secs="));
    }

    #[tokio::test]
    async fn test_acquire_context_bails_with_total_exhausted_branch() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 1; // 不同优先级，确保两个都被尝试

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 两个凭据都设置长冷却
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );
        manager.set_credential_cooldown_with_duration(
            2,
            CooldownReason::ServerError,
            Some(std::time::Duration::from_secs(10)),
        );

        let started = std::time::Instant::now();
        let err = manager.acquire_context().await.err().unwrap();
        let elapsed = started.elapsed();

        assert!(elapsed < std::time::Duration::from_secs(2));
        assert!(err.to_string().contains("所有凭据均处于冷却/速率限制"));
        assert!(err.to_string().contains("retry_after_secs="));
    }

    /// 混合故障场景：一个凭据长冷却，一个凭据 token 刷新失败（access_token/refresh_token 均缺失）。
    /// 期望：不应快速返回 429（会错误吞掉真实的 token 刷新失败语义），应走常规 sleep 路径。
    /// 用 tokio::time::timeout 做短超时，避免测试卡在长 sleep 循环里。
    #[tokio::test]
    async fn test_acquire_context_does_not_bail_429_on_mixed_failures() {
        let config = Config::default();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        // 无 access_token / refresh_token / kiro_api_key —— try_ensure_token 会失败
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = None;
        cred2.refresh_token = None;
        cred2.expires_at = None;
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // cred1 长冷却（超过 2s 阈值），cred2 不设冷却但 token 刷新会失败
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            manager.acquire_context(),
        )
        .await;

        match result {
            Err(_timeout) => {
                // 超时说明进入了 sleep 循环——正是期望的行为（未提前 bail 429）。
            }
            Ok(Ok(_)) => panic!("混合故障场景不应成功获取 context"),
            Ok(Err(err)) => {
                assert!(
                    !err.to_string().contains("所有凭据均处于冷却/速率限制"),
                    "混合故障场景不应 bail 429：{}",
                    err
                );
            }
        }
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_for_user_keeps_affinity_when_bound_credential_rate_limited()
     {
        let mut config = Config::default();
        config.credential_rpm = Some(60_000);

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let first = manager
            .acquire_context_for_user(Some("user-a"), None)
            .await
            .unwrap();
        assert_eq!(first.id, 1);

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(200)),
        );

        let diverted = manager
            .acquire_context_for_user(Some("user-a"), None)
            .await
            .unwrap();
        assert_eq!(diverted.id, 2);

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let while_cooling = manager
            .acquire_context_for_user(Some("user-a"), None)
            .await
            .unwrap();
        assert_eq!(while_cooling.id, 2);

        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        let rebound = manager
            .acquire_context_for_user(Some("user-a"), None)
            .await
            .unwrap();
        assert_eq!(rebound.id, 1);
    }

    #[tokio::test]
    async fn test_acquire_context_scoped_restricts_to_allowed_set() {
        let mut config = Config::default();
        config.credential_rpm = Some(60_000);

        let mk = |tok: &str| {
            let mut c = KiroCredentials::default();
            c.access_token = Some(tok.to_string());
            c.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            c.priority = 0;
            c
        };
        let manager = MultiTokenManager::new(
            config,
            vec![mk("t1"), mk("t2"), mk("t3")],
            None,
            None,
            false,
        )
        .unwrap();

        // 限定只允许 #2：多次获取都必须命中 #2
        let allowed: HashSet<u64> = [2].into_iter().collect();
        for _ in 0..6 {
            let ctx = manager
                .acquire_context_scoped(Some(&allowed))
                .await
                .unwrap();
            assert_eq!(ctx.id, 2);
        }

        // 允许 {1,3}：只会在 1/3 间轮转，绝不选 2
        let allowed_13: HashSet<u64> = [1, 3].into_iter().collect();
        for _ in 0..6 {
            let ctx = manager
                .acquire_context_scoped(Some(&allowed_13))
                .await
                .unwrap();
            assert!(ctx.id == 1 || ctx.id == 3, "got {}", ctx.id);
        }

        // 空作用域（允许集合里全是不存在的 id）：作用域内无可用凭据 → 报错
        let allowed_none: HashSet<u64> = [999].into_iter().collect();
        assert!(
            manager
                .acquire_context_scoped(Some(&allowed_none))
                .await
                .is_err()
        );

        // allowed=None：不受限，可选任意号
        let ctx = manager.acquire_context_scoped(None).await.unwrap();
        assert!((1..=3).contains(&ctx.id));
    }

    #[tokio::test]
    async fn test_acquire_context_for_user_affinity_respects_scope() {
        let mut config = Config::default();
        config.credential_rpm = Some(60_000);
        let mk = |tok: &str, p: u32| {
            let mut c = KiroCredentials::default();
            c.access_token = Some(tok.to_string());
            c.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            c.priority = p;
            c
        };
        // #1 优先级更高，无约束时用户会绑定到 #1
        let manager =
            MultiTokenManager::new(config, vec![mk("t1", 0), mk("t2", 1)], None, None, false)
                .unwrap();

        let first = manager
            .acquire_context_for_user(Some("u"), None)
            .await
            .unwrap();
        assert_eq!(first.id, 1);

        // 现在把该用户限定到只允许 #2：即便亲和性绑的是 #1，也必须改用 #2
        let allowed2: HashSet<u64> = [2].into_iter().collect();
        let scoped = manager
            .acquire_context_for_user(Some("u"), Some(&allowed2))
            .await
            .unwrap();
        assert_eq!(scoped.id, 2);
    }

    // ============ 凭据级 Region 优先级测试 ============

    /// 辅助函数：获取 OIDC 刷新使用的 region（用于测试）
    fn get_oidc_region_for_credential<'a>(
        credentials: &'a KiroCredentials,
        config: &'a Config,
    ) -> &'a str {
        credentials.region.as_ref().unwrap_or(&config.region)
    }

    #[test]
    fn test_build_idc_refresh_user_agents_uses_config_versions() {
        let mut config = Config::default();
        config.system_version = Some("darwin#25.4.0".to_string());
        config.node_version = Some("22.22.0".to_string());

        let (amz_user_agent, user_agent) = build_idc_refresh_user_agents("test-seed", &config);

        assert_eq!(amz_user_agent, "aws-sdk-js/3.980.0 KiroIDE");
        assert!(user_agent.contains("os/darwin#25.4.0"));
        assert!(user_agent.contains("md/nodejs#22.22.0"));
        assert!(user_agent.contains("api/sso-oidc#3.980.0"));
    }

    #[test]
    fn test_build_idc_refresh_user_agents_derives_platform_without_config() {
        // 未配置 system_version 时应从 client_profile 池派生（永不 Linux），保持跨重启稳定
        let config = Config::default();
        let (_, user_agent) = build_idc_refresh_user_agents("stable-seed", &config);
        assert!(
            user_agent.contains("os/darwin#") || user_agent.contains("os/win32#"),
            "derived platform must be mac/win, got: {user_agent}"
        );
        // 同一种子应稳定
        let (_, again) = build_idc_refresh_user_agents("stable-seed", &config);
        assert_eq!(user_agent, again);
    }

    #[test]
    fn test_build_usage_limit_user_agents_uses_config_versions() {
        let mut config = Config::default();
        config.kiro_version = Some("0.11.107".to_string());
        config.system_version = Some("win32#10.0.22631".to_string());
        config.node_version = Some("22.22.0".to_string());
        let credentials = KiroCredentials::default();
        let endpoint = IdeEndpoint::new();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: "machine123",
            config: &config,
        };

        let usage = endpoint.usage_request_parts(&ctx).unwrap();
        let amz_user_agent = usage
            .headers
            .iter()
            .find(|(name, _)| *name == "x-amz-user-agent")
            .map(|(_, value)| value.clone())
            .unwrap();
        let user_agent = usage
            .headers
            .iter()
            .find(|(name, _)| *name == "user-agent")
            .map(|(_, value)| value.clone())
            .unwrap();

        assert_eq!(
            amz_user_agent,
            "aws-sdk-js/1.0.0 KiroIDE-0.11.107-machine123"
        );
        assert!(user_agent.contains("os/win32#10.0.22631"));
        assert!(user_agent.contains("md/nodejs#22.22.0"));
        assert!(user_agent.contains("KiroIDE-0.11.107-machine123"));
    }

    #[test]
    fn test_credential_region_priority_uses_credential_region() {
        // 凭据配置了 region 时，应使用凭据的 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        assert_eq!(region, "eu-west-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_config() {
        // 凭据未配置 region 时，应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let credentials = KiroCredentials::default();
        assert!(credentials.region.is_none());

        let region = get_oidc_region_for_credential(&credentials, &config);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_multiple_credentials_use_respective_regions() {
        // 多凭据场景下，不同凭据使用各自的 region
        let mut config = Config::default();
        config.region = "ap-northeast-1".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.region = Some("us-east-1".to_string());

        let mut cred2 = KiroCredentials::default();
        cred2.region = Some("eu-west-1".to_string());

        let cred3 = KiroCredentials::default(); // 无 region，使用 config

        assert_eq!(get_oidc_region_for_credential(&cred1, &config), "us-east-1");
        assert_eq!(get_oidc_region_for_credential(&cred2, &config), "eu-west-1");
        assert_eq!(
            get_oidc_region_for_credential(&cred3, &config),
            "ap-northeast-1"
        );
    }

    #[test]
    fn test_idc_oidc_endpoint_uses_credential_region() {
        // 验证 IdC OIDC endpoint URL 使用凭据 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-central-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);

        assert_eq!(refresh_url, "https://oidc.eu-central-1.amazonaws.com/token");
    }

    #[test]
    fn test_social_refresh_endpoint_uses_credential_region() {
        // 验证 Social refresh endpoint URL 使用凭据 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("ap-southeast-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);

        assert_eq!(
            refresh_url,
            "https://prod.ap-southeast-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    #[test]
    fn test_api_call_still_uses_config_region() {
        // 验证 API 调用（如 getUsageLimits）仍使用 config.region
        // 这确保只有 OIDC 刷新使用凭据 region，API 调用行为不变
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        // API 调用应使用 config.region，而非 credentials.region
        let api_region = &config.region;
        let api_host = format!("q.{}.amazonaws.com", api_region);

        assert_eq!(api_host, "q.us-west-2.amazonaws.com");
        // 确认凭据 region 不影响 API 调用
        assert_ne!(api_region, credentials.region.as_ref().unwrap());
    }

    #[test]
    fn test_credential_region_empty_string_fallback_to_config() {
        // 空字符串 region 应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("".to_string());

        let region = credentials
            .region
            .as_ref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or(&config.region);
        // 空字符串应回退到 config.region
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_credential_region_whitespace_fallback_to_config() {
        // 纯空白字符 region 应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("   ".to_string());

        let region = credentials
            .region
            .as_ref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or(&config.region);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_update_default_endpoint() {
        let mut config = Config::default();
        config.default_endpoint = "ide".to_string();

        let credentials = KiroCredentials::default();
        let manager = MultiTokenManager::new(config, vec![credentials], None, None, false).unwrap();

        assert_eq!(manager.config().default_endpoint, "ide");

        manager.update_default_endpoint("cli".to_string());
        assert_eq!(manager.config().default_endpoint, "cli");

        manager.update_default_endpoint("ide".to_string());
        assert_eq!(manager.config().default_endpoint, "ide");
    }
}
