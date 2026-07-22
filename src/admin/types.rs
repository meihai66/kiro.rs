//! Admin API 类型定义

use serde::{Deserialize, Deserializer, Serialize};

/// 三态反序列化助手：区分「字段缺省 = 不修改」和「字段为 null = 清空」。
/// 默认 serde 把 null 直接反序列化成 None，与字段缺省无法区分；
/// 这个 helper 把出现的字段（包括 null）包成 Some，缺省字段保持 None。
///
/// 用法：
/// ```ignore
/// #[serde(default, deserialize_with = "deserialize_double_option")]
/// pub credential_rpm: Option<Option<u32>>,
/// ```
pub(crate) fn deserialize_double_option<'de, T, D>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ============ 凭据状态 ============

/// 所有凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 各凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级（数字越小优先级越高）
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    pub disabled_reason: Option<String>,
    /// Token 过期时间（RFC3339 格式）
    pub expires_at: Option<String>,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// refreshToken 的 SHA-256 哈希（用于前端重复检测）
    pub refresh_token_hash: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// 已持久化的订阅等级（页面刷新后可直接展示）
    pub subscription_title: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// API 调用累计失败次数（不随成功清零；与连续 failure_count 区分）
    pub error_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 凭据级 Region（用于 Token 刷新）
    pub region: Option<String>,
    /// 凭据级 API Region（单独覆盖 API 请求）
    pub api_region: Option<String>,
    /// 凭据显式配置的 endpoint（None 表示回退到 defaultEndpoint）
    pub endpoint: Option<String>,
    /// 最终生效的 endpoint 名称
    pub effective_endpoint: String,
    /// 当前绑定的代理槽 ID（启用代理池时；未绑定者不允许启用）
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
    /// 当前冷却原因（None 表示不在冷却中）；前端用来区分「限流/失败」状态
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    /// 当前冷却剩余时间（秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_remaining_secs: Option<u64>,
    /// 凭据级 RPM 上限（None 表示沿用全局 credentialRpm）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_rpm: Option<u32>,
    /// 累计积分消耗（上游 meteringEvent.usage 之和）
    pub credit_usage_total: f64,
    /// 按当前定价换算的累计产出价值（美元）
    pub total_value_usd: f64,
    /// 按模型细分的累计用量与价值（按价值降序）
    pub model_stats: Vec<ModelUsageStat>,
    /// 最近请求结果序列（s=成功 e=失败 r=429，旧 → 新，最多 1000 个字符）
    pub recent_outcomes: String,
}

/// 单个模型的累计用量与价值（展示用）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageStat {
    /// 模型名
    pub model: String,
    /// 累计输入（非缓存）token
    pub input_tokens: u64,
    /// 累计输出 token
    pub output_tokens: u64,
    /// 累计缓存读取 token
    pub cache_read_tokens: u64,
    /// 累计缓存写入 token
    pub cache_write_tokens: u64,
    /// 累计积分
    pub credit_usage: f64,
    /// 累计调用次数
    pub calls: u64,
    /// 按当前定价换算的产出价值（美元）
    pub cost_usd: f64,
}

// ============ 最佳 RPM 分析 ============

/// 单个 RPM 分桶的统计（按每分钟 RPM 落桶聚合）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpmAnalysisBucket {
    /// 桶下界（含）
    pub rpm_low: u32,
    /// 桶上界（不含）
    pub rpm_high: u32,
    /// 落在该桶的样本分钟数
    pub minutes: u32,
    /// 该桶累计请求数（Σ每分钟 RPM）
    pub requests: u64,
    /// 该桶累计 429 数
    pub rl429: u64,
    /// 429 率 = rl429 / requests（requests 为 0 时为 0）
    pub rate429: f64,
}

/// 单个凭据的 RPM 分析结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpmAnalysisEntry {
    /// 凭据 ID
    pub id: u64,
    /// 邮箱（用于前端展示）
    pub email: Option<String>,
    /// 自适应桶宽（RPM）
    pub bucket_width: u32,
    /// 已观测最高 RPM
    pub observed_peak_rpm: u32,
    /// 参与分析的样本分钟总数
    pub total_minutes: u32,
    /// 分桶列表（按 rpmLow 升序）
    pub buckets: Vec<RpmAnalysisBucket>,
}

// ============ 操作请求 ============

/// 启用/禁用凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDisabledRequest {
    /// 是否禁用
    pub disabled: bool,
}

/// 修改优先级请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPriorityRequest {
    /// 新优先级值
    pub priority: u32,
}

/// 修改 Region 请求
///
/// 三态语义（用于支持批量"只改一个"）：
/// - 字段缺省 = 保持不变
/// - 字段为 `null` 或空字符串 = 清除（恢复全局/默认）
/// - 字段为非空字符串 = 设置
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRegionRequest {
    /// 凭据级 Region（用于 Token 刷新）
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub region: Option<Option<String>>,
    /// 凭据级 API Region（单独覆盖 API 请求）
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub api_region: Option<Option<String>>,
}

/// 修改 endpoint 请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEndpointRequest {
    /// endpoint 名称，空字符串或 null 表示回退到 defaultEndpoint
    pub endpoint: Option<String>,
}

/// 修改邮箱请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEmailRequest {
    /// 邮箱（空字符串或 null 表示清除）
    pub email: Option<String>,
}

/// 设置「允许超额使用」开关请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAllowOveruseRequest {
    /// 是否允许超额（true=即使余额用尽也继续派发）
    pub allow: bool,
}

/// 设置单凭据 RPM 上限请求（None 表示清除覆盖，沿用全局 credentialRpm）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialRpmRequest {
    /// RPM 上限；空字符串/0/null 表示清除覆盖
    #[serde(default)]
    pub rpm: Option<u32>,
}

/// 设置超额计费偏好请求（Kiro setUserPreference）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SetOveragePreferenceRequest {
    /// "ENABLED" 或 "DISABLED"（不区分大小写，服务端会规范化）
    pub overage_status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SetOveragePreferenceResponse {
    pub ok: bool,
    pub overage_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 添加凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    pub refresh_token: Option<String>,

    /// Kiro API Key（API Key 凭据必填）
    pub kiro_api_key: Option<String>,

    /// 认证方式（可选，默认 social）
    #[serde(default = "default_auth_method")]
    pub auth_method: String,

    /// OIDC Client ID（IdC 认证需要）
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要）
    pub client_secret: Option<String>,

    /// 优先级（可选，默认 10）
    #[serde(default = "default_import_priority")]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 Token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 API Region（用于 API 调用）
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    pub machine_id: Option<String>,

    /// 凭据级 endpoint（未配置时回退到 config.defaultEndpoint；当前已注册端点由服务端校验）
    pub endpoint: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 是否自动从代理池为新凭据分配槽位（默认 true）；
    /// 启用代理池且 false 时凭据导入后置 disabled=true，需手动 bind
    #[serde(default = "default_true_opt")]
    pub auto_bind_proxy: Option<bool>,

    /// 手动指定要绑定的代理槽 ID（优先级最高，跳过自动选择）
    pub proxy_slot_id: Option<String>,
}

fn default_true_opt() -> Option<bool> {
    Some(true)
}

fn default_auth_method() -> String {
    "social".to_string()
}

/// 添加凭据成功响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    /// 新添加的凭据 ID
    pub credential_id: u64,
    /// 用户邮箱（如果获取成功）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ============ 余额查询 ============

/// 余额查询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 当前使用量
    pub current_usage: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 剩余额度
    pub remaining: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
}

/// 缓存余额信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalanceItem {
    /// 凭据 ID
    pub id: u64,
    /// 缓存的剩余额度
    pub remaining: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 缓存时间（Unix 毫秒时间戳）
    pub cached_at: u64,
    /// 缓存存活时间（秒），缓存过期时间 = cached_at + ttl_secs * 1000
    pub ttl_secs: u64,
}

/// 所有凭据的缓存余额响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalancesResponse {
    /// 各凭据的缓存余额列表
    pub balances: Vec<CachedBalanceItem>,
}

// ============ 负载均衡配置 ============

// ============ 全局代理配置 ============

/// 全局代理配置响应
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfigResponse {
    pub proxy_url: Option<String>,
    pub has_credentials: bool,
}

/// 更新全局代理配置请求
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProxyConfigRequest {
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

// ============ 批量导入 token.json ============

/// 官方 token.json 格式（用于解析导入 / 也作为导出输出 schema）
///
/// 导出时复用此结构以保证「导出文件能直接喂回 import-token-json」。
/// Option 字段使用 `skip_serializing_if` 保持导出 JSON 简洁；代理信息默认不
/// 跟随导出（用户偏好：导入后再手动绑定）。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenJsonItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    /// Kiro API Key（`ksk_*`）；提供时按 api_key 凭据导入，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,
    /// 凭据级端点（`ide` / `cli`），主要供 api_key 凭据使用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default = "default_import_priority")]
    pub priority: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// 邮箱（来自导出文件 account.email；用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// 嵌入式代理（来自 KAM 导出 v1.1+ 的 account.proxy 字段）
    /// 提供时：导入此凭据后将该代理加入代理池并强制绑定，覆盖自动选槽逻辑
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<TokenJsonProxyItem>,
}

/// 凭据导出文件中的嵌入式代理
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TokenJsonProxyItem {
    /// 代理 URL，可包含用户名密码：`socks5://user:pass@host:port`
    pub url: String,
    /// 协议名（仅作记录；以 url scheme 为准）
    #[allow(dead_code)]
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub proxy_type: Option<String>,
    /// 到期时间（接受 RFC3339 / `YYYY-MM-DD` / Unix 秒 / Unix 毫秒）
    #[serde(default, alias = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<serde_json::Value>,
    /// 备注（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// 批量导入请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTokenJsonRequest {
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    pub items: ImportItems,
}

fn default_dry_run() -> bool {
    true
}

fn default_import_priority() -> u32 {
    10
}

/// 导入项（支持单个或数组）
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ImportItems {
    Single(TokenJsonItem),
    Multiple(Vec<TokenJsonItem>),
}

impl ImportItems {
    pub fn into_vec(self) -> Vec<TokenJsonItem> {
        match self {
            ImportItems::Single(item) => vec![item],
            ImportItems::Multiple(items) => items,
        }
    }
}

/// 批量导入响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTokenJsonResponse {
    pub summary: ImportSummary,
    pub items: Vec<ImportItemResult>,
}

/// 导入汇总
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub parsed: usize,
    pub added: usize,
    pub skipped: usize,
    pub invalid: usize,
}

/// 单项导入结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportItemResult {
    pub index: usize,
    pub fingerprint: String,
    pub action: ImportAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
}

/// 导入动作
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportAction {
    Added,
    Skipped,
    Invalid,
}

/// 批量导出请求：按勾选的凭据 ID 导出可重新导入的 JSON
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCredentialsRequest {
    pub credential_ids: Vec<u64>,
}

/// 批量导出响应
///
/// `items` 是 `TokenJsonItem` 数组，可直接作为 `import-token-json` 的
/// `items` 字段（接口本身也接受顶层数组），实现"导出→重新导入"闭环。
/// `skipped` 列出无法导出的 ID 及原因（例如缺少 refreshToken 的 api_key 凭据）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCredentialsResponse {
    pub items: Vec<TokenJsonItem>,
    pub skipped: Vec<ExportSkippedItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSkippedItem {
    pub credential_id: u64,
    pub reason: String,
}

// ============ 代理池 ============

/// 代理池条目（响应；URL 脱敏由前端按需处理）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEntryItem {
    pub id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub remaining_secs: i64,
    pub slots: u32,
    pub used_slots: u32,
    pub bound_credential_ids: Vec<u64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 代理池列表响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyListResponse {
    pub total: usize,
    pub proxies: Vec<ProxyEntryItem>,
    pub enabled: bool,
}

/// 批量导入代理请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProxiesRequest {
    /// 协议（http/https/socks5），默认 http
    #[serde(default = "default_proxy_scheme")]
    pub scheme: String,
    /// 每条代理的槽位容量（默认 1）
    #[serde(default = "default_slots")]
    pub slots_per_proxy: u32,
    /// 统一到期时间（RFC3339）
    pub default_expires_at: chrono::DateTime<chrono::Utc>,
    /// 行格式：每行 host:port:user:pass（user/pass 可空 → 留空段）
    pub lines: Vec<String>,
    /// 可选标签，所有导入条目共享
    #[serde(default)]
    pub label: Option<String>,
}

fn default_proxy_scheme() -> String {
    "http".to_string()
}

fn default_slots() -> u32 {
    1
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProxiesResponse {
    pub total: usize,
    pub added: usize,
    pub failed: usize,
    pub items: Vec<ImportProxyItemResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProxyItemResult {
    pub index: usize,
    pub line: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 批量删除请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxyDeleteRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub force: bool,
}

/// 批量解绑请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxyUnbindRequest {
    pub ids: Vec<String>,
}

/// 批量调槽位请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxySlotsRequest {
    pub ids: Vec<String>,
    pub slots: u32,
    #[serde(default)]
    pub force: bool,
}

/// 批量延长到期时间请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxyExtendRequest {
    pub ids: Vec<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// 批量操作的逐项结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxyItemResult {
    pub id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProxyResponse {
    pub total: usize,
    pub success_count: usize,
    pub fail_count: usize,
    pub items: Vec<BatchProxyItemResult>,
}

/// 凭据 ↔ 代理 绑定请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindProxyRequest {
    pub proxy_id: String,
    /// 绑定后是否自动启用凭据（默认 true）
    #[serde(default = "default_true")]
    pub auto_enable: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyAlertItem {
    pub at: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyAlertsResponse {
    pub total: usize,
    pub alerts: Vec<ProxyAlertItem>,
}

// ============ API Keys ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyItem {
    pub id: i64,
    pub key: String,
    pub key_masked: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub max_concurrent: u32,
    pub cache_read_min_pct: u32,
    pub cache_read_max_pct: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub success_count: u64,
    pub fail_count: u64,
    pub in_flight: u32,
    /// 允许使用的凭据 ID 列表（空 = 全部可用）
    pub allowed_credentials: Vec<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyListResponse {
    pub total: usize,
    pub keys: Vec<ApiKeyItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 自定义 key，留空则自动生成 sk-kiro-... 格式
    #[serde(default)]
    pub custom_key: Option<String>,
    #[serde(default)]
    pub max_concurrent: u32,
    #[serde(default)]
    pub cache_read_min_pct: u32,
    #[serde(default)]
    pub cache_read_max_pct: u32,
    /// 允许使用的凭据 ID 列表（省略或空 = 全部可用）
    #[serde(default)]
    pub allowed_credentials: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApiKeyRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub cache_read_min_pct: Option<u32>,
    #[serde(default)]
    pub cache_read_max_pct: Option<u32>,
    /// 允许使用的凭据范围（Some(列表)=覆盖，空列表=恢复全部可用；省略=不修改）
    #[serde(default)]
    pub allowed_credentials: Option<Vec<u64>>,
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}

// ============ 对话测试 ============

/// 对话测试请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestChatRequest {
    /// Anthropic 模型名（如 claude-opus-4-7）
    pub model: String,
    /// 用户输入文本（默认 "hi"，由前端处理）
    pub message: String,
    /// 可选：限定使用某个凭据 ID（不传则走正常调度）
    #[serde(default)]
    pub credential_id: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestChatResponse {
    /// 实际服务此请求的凭据 ID
    pub credential_id: u64,
    /// 模型回复文本（合并 assistantResponseEvent）
    pub text: String,
    /// 上游耗时（毫秒）
    pub elapsed_ms: u64,
    /// 估算输入 tokens
    pub input_tokens: i32,
    /// 估算输出 tokens
    pub output_tokens: i32,
}

// ============ 错误日志 ============

/// 列表项（不含大字段，用于日志页表格）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorLogSummaryItem {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status: Option<u16>,
    pub error_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_reason: Option<String>,
}

/// 列表响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorLogListResponse {
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
    pub items: Vec<ErrorLogSummaryItem>,
}

/// 详情（包含完整请求/响应）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorLogDetail {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status: Option<u16>,
    pub error_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_reason: Option<String>,
}

/// 列表查询参数（解析自 ?... query string）
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListErrorLogsQuery {
    /// 多状态码用逗号分隔，例如 `429,502`
    #[serde(default)]
    pub status_codes: Option<String>,
    /// 多种 kind 用逗号分隔
    #[serde(default)]
    pub error_kinds: Option<String>,
    #[serde(default)]
    pub credential_id: Option<u64>,
    /// RFC3339 起始时间（含）
    #[serde(default)]
    pub since: Option<String>,
    /// RFC3339 截止时间（含）
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default = "default_log_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_log_limit() -> u32 {
    50
}

/// POST /error-logs/clear 请求体（无字段时清空全部）
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearErrorLogsRequest {
    #[serde(default)]
    pub before: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearErrorLogsResponse {
    pub deleted: u64,
}

// ============ 全局配置 ============

/// 全局配置响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfigResponse {
    /// AWS Region
    pub region: String,
    /// 单凭据目标请求速率（RPM），None 表示无限制
    pub credential_rpm: Option<u32>,
    /// Prompt Cache TTL（秒）
    pub prompt_cache_ttl_seconds: u64,
    /// 是否启用本地 Prompt Cache usage 记账
    pub prompt_cache_accounting_enabled: bool,
    /// cache 比例模拟模式：true=只缩放真实命中的 cache_read（默认）；false=按总输入比例（旧行为）
    pub prompt_cache_sim_scale_hit: bool,
    /// 是否优先采用上游真实输入 token 作为 usage 口径（回退本地估算）
    pub prefer_upstream_input_tokens: bool,
    /// 是否向下游透传上游积分消耗（credit_usage / credit_unit / credit_unit_plural）
    pub expose_credit_usage: bool,
    /// prompt cache 命名空间是否按 API Key 划分（缓存池）；false=按凭据隔离
    pub prompt_cache_api_key_pool: bool,
    /// 默认端点名称（凭据未显式指定 endpoint 时使用）
    pub default_endpoint: String,
    /// 压缩配置
    pub compression: CompressionConfigResponse,
    /// 错误响应自动禁用规则（一行一条；body 含其中任一字符串则自动禁用凭据）
    pub auto_disable_patterns: Vec<String>,
    /// 错误内容替换规则（一行一条；格式 `pattern===replacement`）
    pub error_replace_rules: Vec<String>,
    /// 使用率达到此百分比时自动禁用（0=不启用）
    pub auto_disable_usage_threshold_pct: u32,
    /// 单凭据最多重试次数
    pub max_retries_per_credential: u32,
    /// 单请求总重试次数硬上限
    pub max_total_retries: u32,
    /// "所有凭据均处于冷却"时立即返回 429 的等待阈值（秒）；0 表示永不快速 bail
    pub all_credentials_cooldown_bail_threshold_secs: u64,
    /// 是否启用 MODEL_TEMPORARILY_UNAVAILABLE 全局熔断
    pub model_unavailable_breaker_enabled: bool,
    /// 新导入凭据是否默认禁用
    pub import_disabled_by_default: bool,
    /// 余额自动刷新目标周期（秒）；0 = 禁用
    pub balance_auto_refresh_secs: u32,
    /// 余额刷新并发数（后台 + 启动初始化）
    pub balance_refresh_concurrency: u32,
    /// 触发 429 限流时的最短冷却（秒）
    pub rate_limit_cooldown_min_secs: u64,
    /// 触发 429 限流时的最长冷却（秒）
    pub rate_limit_cooldown_max_secs: u64,
    /// 容量瓶颈型 429 的冷却（秒）
    pub capacity_pressure_cooldown_secs: u64,
    /// 是否忽略上游 Retry-After，直接在 [min,max] 内随机出冷却
    pub rate_limit_ignore_retry_after: bool,
    /// 严格遵循上游 Retry-After（带头时按原值冷却，不 clamp 不随机）
    pub rate_limit_follow_retry_after: bool,
    /// 全局关闭 429 冷却（开启后 429 只触发换号重试，不让凭据进入冷却）
    pub rate_limit_disable_cooldown: bool,
    /// 错误日志开关
    pub error_log_enabled: bool,
    /// 错误日志最大保留条数（0=不限）
    pub error_log_max_count: u32,
    /// 错误日志最大保留天数（0=不限）
    pub error_log_max_age_days: u32,
    /// 不记录的 HTTP 状态码黑名单
    pub error_log_excluded_status_codes: Vec<u16>,
    /// 模型定价（用于「产出价值」统计）
    pub pricing: crate::model::config::PricingConfig,
    /// 模型映射（请求模型名 → 上游 Kiro 模型 ID；为空则用内置映射）
    pub model_mapping: crate::model::config::ModelMappingConfig,
    /// `/v1/models` 自定义模型列表（为空则用内置列表）
    pub models: Vec<crate::model::config::ModelEntry>,
    /// 提醒推送配置（阈值告警推送）
    pub push_notification: crate::model::config::PushNotificationConfig,
}

/// 压缩配置响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfigResponse {
    pub enabled: bool,
    pub whitespace_compression: bool,
    pub thinking_strategy: String,
    pub tool_result_max_chars: usize,
    pub tool_result_head_lines: usize,
    pub tool_result_tail_lines: usize,
    pub tool_use_input_max_chars: usize,
    pub tool_description_max_chars: usize,
    pub max_history_turns: usize,
    pub max_history_chars: usize,
    pub max_request_body_bytes: usize,
    /// 按模型的请求体大小上限覆盖规则
    pub per_model_body_limits: Vec<crate::model::config::ModelBodyLimitRule>,
}

/// 更新全局配置请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGlobalConfigRequest {
    /// AWS Region（可选）
    pub region: Option<String>,
    /// 单凭据目标请求速率（RPM，可选）。
    /// `null` 表示清除（无限制），字段缺省表示不修改。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub credential_rpm: Option<Option<u32>>,
    /// Prompt Cache TTL（秒，可选，仅支持 300 或 3600）
    pub prompt_cache_ttl_seconds: Option<u64>,
    /// 是否启用本地 Prompt Cache usage 记账（可选）
    pub prompt_cache_accounting_enabled: Option<bool>,
    /// cache 比例模拟模式（可选）：true=只缩放真实命中；false=按总输入比例（旧行为）
    pub prompt_cache_sim_scale_hit: Option<bool>,
    /// 是否优先采用上游真实输入 token 作为 usage 口径（可选）
    pub prefer_upstream_input_tokens: Option<bool>,
    /// 是否向下游透传上游积分消耗（可选）
    pub expose_credit_usage: Option<bool>,
    /// prompt cache 命名空间是否按 API Key 划分（可选）
    pub prompt_cache_api_key_pool: Option<bool>,
    /// 默认端点名称（可选）
    pub default_endpoint: Option<String>,
    /// 压缩配置（可选）
    pub compression: Option<UpdateCompressionConfigRequest>,
    /// 错误响应自动禁用规则（可选；提供则整体替换）
    pub auto_disable_patterns: Option<Vec<String>>,
    /// 错误内容替换规则（可选；提供则整体替换）
    pub error_replace_rules: Option<Vec<String>>,
    /// 使用率达到此百分比时自动禁用（0=关闭；可选）
    pub auto_disable_usage_threshold_pct: Option<u32>,
    /// 单凭据最多重试次数（可选）
    pub max_retries_per_credential: Option<u32>,
    /// 单请求总重试次数硬上限（可选）
    pub max_total_retries: Option<u32>,
    /// "所有凭据均处于冷却"时立即返回 429 的等待阈值（秒，可选；0=禁用快速 bail）
    pub all_credentials_cooldown_bail_threshold_secs: Option<u64>,
    /// 是否启用 MODEL_TEMPORARILY_UNAVAILABLE 全局熔断（可选）
    pub model_unavailable_breaker_enabled: Option<bool>,
    /// 新导入凭据是否默认禁用
    pub import_disabled_by_default: Option<bool>,
    /// 余额自动刷新目标周期（秒，可选；0=禁用）
    pub balance_auto_refresh_secs: Option<u32>,
    /// 余额刷新并发数（可选；1~256）
    pub balance_refresh_concurrency: Option<u32>,
    /// 触发 429 限流时的最短冷却（秒，可选）
    pub rate_limit_cooldown_min_secs: Option<u64>,
    /// 触发 429 限流时的最长冷却（秒，可选）
    pub rate_limit_cooldown_max_secs: Option<u64>,
    /// 容量瓶颈型 429 的冷却（秒，可选）
    pub capacity_pressure_cooldown_secs: Option<u64>,
    /// 忽略上游 Retry-After 直接随机（可选）
    pub rate_limit_ignore_retry_after: Option<bool>,
    /// 严格遵循上游 Retry-After（可选；带头时按原值冷却，不 clamp 不随机）
    pub rate_limit_follow_retry_after: Option<bool>,
    /// 全局关闭 429 冷却（可选；开启后所有 429 不再让凭据进入冷却）
    pub rate_limit_disable_cooldown: Option<bool>,
    /// 错误日志开关
    pub error_log_enabled: Option<bool>,
    /// 错误日志最大保留条数
    pub error_log_max_count: Option<u32>,
    /// 错误日志最大保留天数
    pub error_log_max_age_days: Option<u32>,
    /// 不记录的 HTTP 状态码黑名单（提供则整体替换；空数组表示不排除任何状态码）
    pub error_log_excluded_status_codes: Option<Vec<u16>>,
    /// 模型定价（可选；提供则整体替换）
    pub pricing: Option<crate::model::config::PricingConfig>,
    /// 模型映射（可选；提供则整体替换规则列表）
    pub model_mapping: Option<crate::model::config::ModelMappingConfig>,
    /// `/v1/models` 自定义模型列表（可选；提供则整体替换，空数组恢复内置列表）
    pub models: Option<Vec<crate::model::config::ModelEntry>>,
    /// 提醒推送配置（可选；提供则整体替换）
    pub push_notification: Option<crate::model::config::PushNotificationConfig>,
}

/// 更新压缩配置请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCompressionConfigRequest {
    pub enabled: Option<bool>,
    pub whitespace_compression: Option<bool>,
    pub thinking_strategy: Option<String>,
    pub tool_result_max_chars: Option<usize>,
    pub tool_result_head_lines: Option<usize>,
    pub tool_result_tail_lines: Option<usize>,
    pub tool_use_input_max_chars: Option<usize>,
    pub tool_description_max_chars: Option<usize>,
    pub max_history_turns: Option<usize>,
    pub max_history_chars: Option<usize>,
    pub max_request_body_bytes: Option<usize>,
    /// 按模型的请求体大小上限覆盖规则（提供则整体替换）
    pub per_model_body_limits: Option<Vec<crate::model::config::ModelBodyLimitRule>>,
}
