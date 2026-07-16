use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    #[default]
    Rustls,
    NativeTls,
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 单个凭据的目标请求速率（RPM，每分钟请求数）
    ///
    /// 用于凭据级节流/分流：当某个凭据短时间内请求过密时，优先将流量分配到其他可用凭据，
    /// 从而减少上游 429 的概率。
    ///
    /// - `None` 或 `0`: 使用内置默认节流策略
    /// - `>0`: 将最小/最大请求间隔固定为 `60_000 / rpm` 毫秒
    #[serde(default)]
    pub credential_rpm: Option<u32>,

    /// 输入压缩配置
    #[serde(default)]
    pub compression: CompressionConfig,

    /// Prompt Cache TTL（秒），默认 300 秒
    #[serde(default = "default_prompt_cache_ttl_seconds")]
    pub prompt_cache_ttl_seconds: u64,

    /// 是否启用本地 Prompt Cache usage 记账，默认 true
    #[serde(default = "default_true")]
    pub prompt_cache_accounting_enabled: bool,

    /// cache 比例模拟模式（仅当 API Key 配置了 cacheRead pct 时生效）：
    /// true=只缩放真实命中的 cache_read（命中才处理，默认）；
    /// false=按总输入比例切分给 cache_read（旧行为，不论是否命中）。
    #[serde(default = "default_true")]
    pub prompt_cache_sim_scale_hit: bool,

    /// 是否优先采用上游真实输入 token（contextUsageEvent）作为 usage 口径，
    /// 上游未返回时回退本地估算。默认 false（保持本地估算口径）
    #[serde(default)]
    pub prefer_upstream_input_tokens: bool,

    /// 是否在响应 usage 中向下游透传上游 Kiro 积分消耗
    /// （credit_usage / credit_unit / credit_unit_plural 三个非标字段）。
    /// false 时对下游剥离，管理后台的内部积分统计不受影响。默认 true（保持现状）
    #[serde(default = "default_true")]
    pub expose_credit_usage: bool,

    /// prompt cache 命名空间是否按 API Key 划分（缓存池）。
    /// true：同一 API Key 的请求共享一个缓存池，多凭据轮换不再造成缓存 miss；
    /// false：按上游凭据隔离（现状，凭据轮换时会整段重计 cache 创建）。默认 false
    #[serde(default)]
    pub prompt_cache_api_key_pool: bool,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 是否启用代理池（启用后所有出站强制走凭据所绑代理，不允许回退本地直连）
    #[serde(default)]
    pub proxy_pool_enabled: bool,

    /// 代理池文件路径（默认 ./proxies.json，命令行 --proxies 优先）
    #[serde(default)]
    pub proxy_pool_path: Option<String>,

    /// 提前轮换阈值（小时），剩余有效期 < 此值则触发后台轮换。默认 24
    #[serde(default = "default_proxy_expiry_warning_hours")]
    pub proxy_expiry_warning_hours: i64,

    /// 后台轮换扫描间隔（秒）。默认 60
    #[serde(default = "default_proxy_rotation_interval_seconds")]
    pub proxy_rotation_interval_seconds: u64,

    /// SQLite 数据库文件路径（默认 ./kiro.db）
    #[serde(default)]
    pub db_path: Option<String>,

    /// 错误响应自动禁用规则：响应 body 含其中任一字符串则自动禁用对应凭据
    /// 一行一条；空字符串忽略
    #[serde(default)]
    pub auto_disable_patterns: Vec<String>,

    /// 错误内容替换规则：每条形如 `pattern===replacement`
    ///
    /// 命中 pattern 时优先把响应 JSON 里的错误原因字段（`message` / `error.message` 等）的值整体换成 replacement，
    /// 保留外层结构；若不是 JSON 或找不到 message 字段则整段替换。
    ///
    /// 一行一条；空字符串忽略；只影响返回给客户端的错误体，不影响内部禁用/重试逻辑。
    #[serde(default)]
    pub error_replace_rules: Vec<String>,

    /// 当凭据使用率达到此百分比时自动禁用（0~100；0=不启用）
    #[serde(default)]
    pub auto_disable_usage_threshold_pct: u32,

    /// 新导入的凭据是否默认禁用（需手动启用才能参与调度）。
    /// 默认 true——避免新号未验证就直接进调度池踩雷。
    #[serde(default = "default_true")]
    pub import_disabled_by_default: bool,

    /// 余额自动刷新目标周期（秒）；0 = 禁用。
    /// 后台滚动刷新每个凭据，平均每 N 秒打一次上游 getUsageLimits。
    /// 推荐 600~900（10-15 分钟）。
    #[serde(default)]
    pub balance_auto_refresh_secs: u32,

    /// 余额刷新并发数（后台自动刷新 + 启动初始化）。默认 8。
    /// 每个凭据走各自代理出口时可调高（如 32+）以加速大量号的刷新；
    /// 若多个凭据共用同一出口 IP，调高可能触发上游 429，请谨慎。
    #[serde(default = "default_balance_refresh_concurrency")]
    pub balance_refresh_concurrency: u32,

    /// 触发 429 限流时的最短冷却（秒）。
    /// 上游若返回 Retry-After 会优先使用，但不会低于此值；
    /// 上游未带 Retry-After 时直接用此值。默认 60。
    #[serde(default = "default_rate_limit_cooldown_min_secs")]
    pub rate_limit_cooldown_min_secs: u64,

    /// 触发 429 限流时的最长冷却（秒）。
    /// 用于把异常的 Retry-After（比如几小时）clamp 下来，避免单号被挂死。默认 300。
    #[serde(default = "default_rate_limit_cooldown_max_secs")]
    pub rate_limit_cooldown_max_secs: u64,

    /// 容量瓶颈型 429（INSUFFICIENT_MODEL_CAPACITY / high traffic）的冷却（秒）。
    /// 这类不是单号被限流，是上游瞬时容量不够；用短冷却避免把所有号一并 60s 锁死。
    /// 默认 8。
    #[serde(default = "default_capacity_pressure_cooldown_secs")]
    pub capacity_pressure_cooldown_secs: u64,

    /// 是否忽略上游 Retry-After，直接在 [min, max] 内随机出冷却时长。
    /// 默认 false（仍优先尊重上游）。开启后避开上游"被限号"集体长冷却的问题。
    /// 仅作用于普通 429；容量类 429（INSUFFICIENT_MODEL_CAPACITY）仍走 capacity 短冷却。
    #[serde(default)]
    pub rate_limit_ignore_retry_after: bool,

    /// 全局关闭 429 冷却开关。
    /// 开启后所有 429（包括容量类）都不会让凭据进入冷却状态——只会触发一次"换号重试"，
    /// 下一次轮询时该凭据立即可被再次选中。默认 false（保留冷却保护）。
    /// 适用于「上游 429 已经只是软限流、不想让任何号被锁住」的场景。
    #[serde(default)]
    pub rate_limit_disable_cooldown: bool,

    /// 错误日志总开关（关闭后不写库；接口仍可读历史日志）
    #[serde(default = "default_true")]
    pub error_log_enabled: bool,

    /// 错误日志最大保留条数（0 = 不限）
    #[serde(default = "default_error_log_max_count")]
    pub error_log_max_count: u32,

    /// 错误日志最大保留天数（0 = 不限）
    #[serde(default = "default_error_log_max_age_days")]
    pub error_log_max_age_days: u32,

    /// 不记录的状态码黑名单（如 [400, 401]）
    #[serde(default)]
    pub error_log_excluded_status_codes: Vec<u16>,

    /// 单凭据最多重试次数（0 视为禁用重试，按未配置取默认 2）
    #[serde(default = "default_max_retries_per_credential")]
    pub max_retries_per_credential: u32,

    /// 单请求总重试次数硬上限（默认 3）
    #[serde(default = "default_max_total_retries")]
    pub max_total_retries: u32,

    /// "所有凭据均处于冷却"时立即返回 429 的等待阈值（秒）；
    /// 最短可用等待 ≤ 该阈值时短睡再试；> 该阈值则立即 429+Retry-After 给客户端。
    /// 默认 2 秒；0 视为不启用快速 bail（永远短睡到可用）。
    #[serde(default = "default_all_credentials_cooldown_bail_threshold_secs")]
    pub all_credentials_cooldown_bail_threshold_secs: u64,

    /// 是否启用 MODEL_TEMPORARILY_UNAVAILABLE 全局熔断。
    /// 默认 true：上游连续返回该错误达到内部阈值后，临时禁用所有凭据，5 分钟后自动恢复。
    /// 关闭后即使上游连续返回此错误也不会触发全局禁用，仅依赖单凭据故障转移和重试。
    #[serde(default = "default_true")]
    pub model_unavailable_breaker_enabled: bool,

    /// 模型定价（用于「凭据产出价值」统计）。
    /// 仅影响展示侧的价值换算，不影响任何代理/计费逻辑。
    #[serde(default)]
    pub pricing: PricingConfig,

    /// 模型映射（请求的模型名 → 上游 Kiro 模型 ID）。
    /// 为空（默认）时回退到内置映射；非空时完全接管，未命中即「模型不存在」。
    #[serde(default)]
    pub model_mapping: ModelMappingConfig,

    /// `/v1/models` 返回的自定义模型列表（设置页可编辑）。
    /// 为空（默认）时返回内置模型列表；非空时完全接管。
    #[serde(default)]
    pub models: Vec<ModelEntry>,

    /// 提醒推送配置（阈值告警推送到 ogpush）
    #[serde(default)]
    pub push_notification: PushNotificationConfig,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

/// 提醒推送配置：可用凭据数 / 预计可用时长 低于阈值时，调用 ogpush 推送接口告警。
///
/// 两个阈值独立启用（0 = 不检查该项）；同一轮检查两项同时触发只合并推送一条；
/// 相邻两次推送之间固定间隔至少 30 分钟（硬编码，不可配置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PushNotificationConfig {
    /// 总开关
    pub enabled: bool,
    /// 推送接口地址
    pub api_url: String,
    /// X-API-Key 密钥
    pub api_key: String,
    /// 收件用户组 id 列表（与 user_ids / usernames 至少一个非空）
    pub group_ids: Vec<i64>,
    /// 收件用户 id 列表
    pub user_ids: Vec<i64>,
    /// 收件用户名列表
    pub usernames: Vec<String>,
    /// 优先级：normal（响一声）| urgent（全屏弹出 + 循环响铃）
    pub priority: String,
    /// 可用凭据数低于此值时推送（0 = 不检查）
    pub min_available_credentials: u32,
    /// 预计可用时长低于此分钟数时推送（0 = 不检查）
    pub min_remaining_minutes: u32,
    /// 估算消耗速率的采样窗口（分钟），默认 5
    pub credit_window_minutes: u32,
}

impl Default for PushNotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_url: default_push_api_url(),
            api_key: String::new(),
            group_ids: Vec::new(),
            user_ids: Vec::new(),
            usernames: Vec::new(),
            priority: "normal".to_string(),
            min_available_credentials: 0,
            min_remaining_minutes: 0,
            credit_window_minutes: 5,
        }
    }
}

impl PushNotificationConfig {
    /// 是否配置了至少一个收件人
    pub fn has_recipients(&self) -> bool {
        !self.group_ids.is_empty() || !self.user_ids.is_empty() || !self.usernames.is_empty()
    }
}

fn default_push_api_url() -> String {
    "https://ogpush.ogog.dev/api/push".to_string()
}

/// 单档价格（单位：美元 / 每百万 token）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PricingRate {
    /// 输入（非缓存）单价
    #[serde(default)]
    pub input: f64,
    /// 输出单价
    #[serde(default)]
    pub output: f64,
    /// 缓存读取单价
    #[serde(default)]
    pub cache_read: f64,
    /// 缓存写入单价
    #[serde(default)]
    pub cache_write: f64,
}

/// 一条定价匹配规则（自上而下第一条命中者生效）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingRule {
    /// 展示用标签（如 "Opus"）
    #[serde(default)]
    pub label: String,
    /// 匹配串
    #[serde(rename = "match", default)]
    pub pattern: String,
    /// 匹配方式：exact | prefix | contains | glob
    #[serde(default = "default_match_type")]
    pub match_type: String,
    /// 价格档
    #[serde(flatten)]
    pub rate: PricingRate,
}

impl PricingRule {
    /// 判断模型名是否命中本规则（大小写不敏感）
    pub fn matches(&self, model: &str) -> bool {
        let p = self.pattern.trim();
        if p.is_empty() {
            return false;
        }
        let model_l = model.to_ascii_lowercase();
        let pat_l = p.to_ascii_lowercase();
        match self.match_type.as_str() {
            "exact" => model_l == pat_l,
            "prefix" => model_l.starts_with(&pat_l),
            "glob" => glob_match(&pat_l, &model_l),
            // 默认含「contains」
            _ => model_l.contains(&pat_l),
        }
    }
}

/// 模型定价配置：有序规则 + 兜底默认价
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingConfig {
    /// 有序匹配规则
    #[serde(default)]
    pub rules: Vec<PricingRule>,
    /// 未命中任何规则时的兜底价（默认全 0）
    #[serde(default)]
    pub default: PricingRate,
    /// 全局倍率：所有模型算出的价值最终都 × 此值（默认 1.0）
    #[serde(default = "default_pricing_multiplier")]
    pub multiplier: f64,
}

fn default_pricing_multiplier() -> f64 {
    1.0
}

impl Default for PricingConfig {
    fn default() -> Self {
        // 内置 Anthropic 官网定价（美元 / 每百万 token），用户可在设置页覆盖
        let rule = |label: &str, pat: &str, i: f64, o: f64, cr: f64, cw: f64| PricingRule {
            label: label.to_string(),
            pattern: pat.to_string(),
            match_type: "contains".to_string(),
            rate: PricingRate {
                input: i,
                output: o,
                cache_read: cr,
                cache_write: cw,
            },
        };
        Self {
            rules: vec![
                rule("Opus", "opus", 15.0, 75.0, 1.5, 18.75),
                rule("Sonnet", "sonnet", 3.0, 15.0, 0.3, 3.75),
                rule("Haiku", "haiku", 1.0, 5.0, 0.1, 1.25),
            ],
            default: PricingRate::default(),
            multiplier: default_pricing_multiplier(),
        }
    }
}

impl PricingConfig {
    /// 取某模型生效的价格档（自上而下第一条命中；都不中则用 default）
    pub fn rate_for(&self, model: &str) -> &PricingRate {
        self.rules
            .iter()
            .find(|r| r.matches(model))
            .map(|r| &r.rate)
            .unwrap_or(&self.default)
    }

    /// 按 token 用量算出该次（或累计）的美元价值
    pub fn cost_usd(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        let r = self.rate_for(model);
        let base = (input_tokens as f64 * r.input
            + output_tokens as f64 * r.output
            + cache_read_tokens as f64 * r.cache_read
            + cache_write_tokens as f64 * r.cache_write)
            / 1_000_000.0;
        base * self.multiplier
    }
}

fn default_match_type() -> String {
    "contains".to_string()
}

/// 一条模型映射规则（自上而下第一条命中者生效）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingRule {
    /// 展示用标签（如 "Opus 4.8"）
    #[serde(default)]
    pub label: String,
    /// 匹配串（匹配请求里的模型名）
    #[serde(rename = "match", default)]
    pub pattern: String,
    /// 匹配方式：exact | prefix | contains | glob
    #[serde(default = "default_match_type")]
    pub match_type: String,
    /// 命中后实际发给上游的 Kiro 模型 ID（如 "claude-opus-4.8"）
    #[serde(default)]
    pub model: String,
}

impl ModelMappingRule {
    /// 判断模型名是否命中本规则（大小写不敏感）
    pub fn matches(&self, model: &str) -> bool {
        let p = self.pattern.trim();
        if p.is_empty() {
            return false;
        }
        let model_l = model.to_ascii_lowercase();
        let pat_l = p.to_ascii_lowercase();
        match self.match_type.as_str() {
            "exact" => model_l == pat_l,
            "prefix" => model_l.starts_with(&pat_l),
            "glob" => glob_match(&pat_l, &model_l),
            // 默认含「contains」
            _ => model_l.contains(&pat_l),
        }
    }
}

/// 模型映射配置：有序规则，自上而下第一条命中者生效。
///
/// 为空时上层回退到内置映射；非空时完全接管：未命中任何规则即视为「模型不存在」，
/// 不再回退内置默认——由使用方（converter）决定回退语义，本结构只做规则匹配。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingConfig {
    /// 有序匹配规则
    #[serde(default)]
    pub rules: Vec<ModelMappingRule>,
}

impl ModelMappingConfig {
    /// 是否未配置任何规则
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 按序匹配，返回命中规则的目标 Kiro 模型 ID；无规则或都不命中返回 `None`。
    /// 命中规则但目标为空串同样视为未命中（配置无效）。
    pub fn resolve(&self, model: &str) -> Option<String> {
        self.rules
            .iter()
            .find(|r| r.matches(model))
            .map(|r| r.model.trim().to_string())
            .filter(|m| !m.is_empty())
    }
}

/// 一条「按模型的请求体大小上限」规则（自上而下第一条命中者生效）
///
/// 命中后用其 `max_bytes` 覆盖全局 [`CompressionConfig::max_request_body_bytes`]，
/// 让不同模型可配不同的请求体上限。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBodyLimitRule {
    /// 展示用标签（如 "Opus"）
    #[serde(default)]
    pub label: String,
    /// 匹配串（匹配请求里的模型名）
    #[serde(rename = "match", default)]
    pub pattern: String,
    /// 匹配方式：exact | prefix | contains | glob
    #[serde(default = "default_match_type")]
    pub match_type: String,
    /// 命中后该模型的请求体最大字节数（0 = 不限制）
    #[serde(default)]
    pub max_bytes: usize,
}

impl ModelBodyLimitRule {
    /// 判断模型名是否命中本规则（大小写不敏感，与 [`ModelMappingRule::matches`] 同规则）
    pub fn matches(&self, model: &str) -> bool {
        let p = self.pattern.trim();
        if p.is_empty() {
            return false;
        }
        let model_l = model.to_ascii_lowercase();
        let pat_l = p.to_ascii_lowercase();
        match self.match_type.as_str() {
            "exact" => model_l == pat_l,
            "prefix" => model_l.starts_with(&pat_l),
            "glob" => glob_match(&pat_l, &model_l),
            _ => model_l.contains(&pat_l),
        }
    }
}

/// `/v1/models` 的一条自定义模型条目。
///
/// 只暴露常用字段；其余响应字段（object / owned_by / type 等）由 handler 填固定值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// 模型 ID（如 "claude-opus-4-8"）
    pub id: String,
    /// 展示名；留空时直接用 id
    #[serde(default)]
    pub display_name: String,
    /// 上下文长度；0 = 响应中省略该字段
    #[serde(default)]
    pub context_length: i32,
    /// 最大输出 tokens；0 = 响应中省略该字段
    #[serde(default)]
    pub max_completion_tokens: i32,
}

/// 极简 glob 匹配：仅支持 `*`（任意串）与 `?`（任意单字符），其余字面匹配。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // 经典 DP / 双指针回溯
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn default_rate_limit_cooldown_min_secs() -> u64 {
    60
}

fn default_rate_limit_cooldown_max_secs() -> u64 {
    300
}

fn default_capacity_pressure_cooldown_secs() -> u64 {
    8
}

fn default_error_log_max_count() -> u32 {
    50_000
}

fn default_error_log_max_age_days() -> u32 {
    7
}

fn default_max_retries_per_credential() -> u32 {
    2
}

fn default_max_total_retries() -> u32 {
    3
}

fn default_all_credentials_cooldown_bail_threshold_secs() -> u64 {
    2
}

fn default_balance_refresh_concurrency() -> u32 {
    8
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "0.11.107".to_string()
}

fn default_system_version() -> String {
    const SYSTEM_VERSIONS: &[&str] = &["darwin#24.6.0", "win32#10.0.22631"];
    SYSTEM_VERSIONS[fastrand::usize(..SYSTEM_VERSIONS.len())].to_string()
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_endpoint() -> String {
    "ide".to_string()
}

fn default_proxy_expiry_warning_hours() -> i64 {
    24
}

fn default_proxy_rotation_interval_seconds() -> u64 {
    60
}

fn default_prompt_cache_ttl_seconds() -> u64 {
    300
}

fn default_tls_backend() -> TlsBackend {
    // 默认 native-tls（vendored openssl）：与真实 Kiro IDE 客户端
    // （Electron + Node OpenSSL）TLS 指纹更接近，降低 JA3/JA4 风控概率。
    // 受限环境可改 rustls，但需注意指纹差异。
    TlsBackend::NativeTls
}

fn default_true() -> bool {
    true
}

fn default_thinking_strategy() -> String {
    "discard".to_string()
}

fn default_8000() -> usize {
    8000
}

fn default_80() -> usize {
    80
}

fn default_40() -> usize {
    40
}

fn default_6000() -> usize {
    6000
}

fn default_4000() -> usize {
    4000
}

fn default_80_turns() -> usize {
    80
}

fn default_400k() -> usize {
    400_000
}

fn default_image_max_long_edge() -> u32 {
    4000
}

fn default_image_max_pixels_single() -> u32 {
    4_000_000
}

fn default_image_max_pixels_multi() -> u32 {
    4_000_000
}

fn default_image_multi_threshold() -> usize {
    20
}

fn default_max_request_body_bytes() -> usize {
    // 上游对请求体大小存在硬性限制（实测约 5MiB 左右会触发 400），
    // 这里默认设置为 4.5MiB 留出安全余量。
    4_718_592
}

/// 输入压缩配置
///
/// 控制请求体在协议转换后、发送到上游前的多层压缩策略。
/// 所有阈值均可通过配置文件调整，默认开启。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfig {
    /// 总开关，默认 true
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 空白压缩（连续空行、行尾空格），默认 true
    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
    /// thinking 块处理策略: "discard" | "truncate" | "keep"
    #[serde(default = "default_thinking_strategy")]
    pub thinking_strategy: String,
    /// tool_result 截断阈值（字符数），默认 8000
    #[serde(default = "default_8000")]
    pub tool_result_max_chars: usize,
    /// 智能截断保留头部行数，默认 80
    #[serde(default = "default_80")]
    pub tool_result_head_lines: usize,
    /// 智能截断保留尾部行数，默认 40
    #[serde(default = "default_40")]
    pub tool_result_tail_lines: usize,
    /// tool_use input 截断阈值（字符数），默认 6000
    #[serde(default = "default_6000")]
    pub tool_use_input_max_chars: usize,
    /// 工具描述截断阈值（字符数），覆盖原 10000 硬编码，默认 4000
    #[serde(default = "default_4000")]
    pub tool_description_max_chars: usize,
    /// 历史最大轮数，默认 80（0=不限）
    #[serde(default = "default_80_turns")]
    pub max_history_turns: usize,
    /// 历史最大字符数，默认 400000（0=不限）
    #[serde(default = "default_400k")]
    pub max_history_chars: usize,
    /// 图片长边最大像素，默认 4000（Anthropic 硬限制 8000，留安全余量；窄长图受益于更大长边）
    #[serde(default = "default_image_max_long_edge")]
    pub image_max_long_edge: u32,
    /// 单张图片最大总像素，默认 4_000_000（2000×2000，与多图限制一致）
    #[serde(default = "default_image_max_pixels_single")]
    pub image_max_pixels_single: u32,
    /// 多图模式下单张图片最大总像素，默认 4_000_000（2000×2000）
    #[serde(default = "default_image_max_pixels_multi")]
    pub image_max_pixels_multi: u32,
    /// 触发多图限制的图片数量阈值，默认 20
    #[serde(default = "default_image_multi_threshold")]
    pub image_multi_threshold: usize,
    /// 请求体最大字节数，超过则直接拒绝（0 = 不限制）
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: usize,
    /// 按模型的请求体大小上限覆盖规则（自上而下第一条命中生效；
    /// 未命中任何规则时回退到全局 `max_request_body_bytes`）
    #[serde(default)]
    pub per_model_body_limits: Vec<ModelBodyLimitRule>,
}

impl CompressionConfig {
    /// 解析指定模型实际生效的请求体大小上限（字节）。
    ///
    /// 优先匹配 `per_model_body_limits`（有序，第一条命中生效），
    /// 未命中则回退全局 `max_request_body_bytes`。
    pub fn resolve_max_body_bytes(&self, model: &str) -> usize {
        self.per_model_body_limits
            .iter()
            .find(|r| r.matches(model))
            .map(|r| r.max_bytes)
            .unwrap_or(self.max_request_body_bytes)
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            whitespace_compression: true,
            thinking_strategy: default_thinking_strategy(),
            tool_result_max_chars: default_8000(),
            tool_result_head_lines: default_80(),
            tool_result_tail_lines: default_40(),
            tool_use_input_max_chars: default_6000(),
            tool_description_max_chars: default_4000(),
            max_history_turns: default_80_turns(),
            max_history_chars: default_400k(),
            image_max_long_edge: default_image_max_long_edge(),
            image_max_pixels_single: default_image_max_pixels_single(),
            image_max_pixels_multi: default_image_max_pixels_multi(),
            image_multi_threshold: default_image_multi_threshold(),
            max_request_body_bytes: default_max_request_body_bytes(),
            per_model_body_limits: Vec::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            credential_rpm: None,
            compression: CompressionConfig::default(),
            prompt_cache_ttl_seconds: default_prompt_cache_ttl_seconds(),
            prompt_cache_accounting_enabled: default_true(),
            prompt_cache_sim_scale_hit: default_true(),
            prefer_upstream_input_tokens: false,
            expose_credit_usage: default_true(),
            prompt_cache_api_key_pool: false,
            default_endpoint: default_endpoint(),
            proxy_pool_enabled: false,
            proxy_pool_path: None,
            proxy_expiry_warning_hours: default_proxy_expiry_warning_hours(),
            proxy_rotation_interval_seconds: default_proxy_rotation_interval_seconds(),
            db_path: None,
            auto_disable_patterns: Vec::new(),
            error_replace_rules: Vec::new(),
            auto_disable_usage_threshold_pct: 0,
            max_retries_per_credential: default_max_retries_per_credential(),
            max_total_retries: default_max_total_retries(),
            all_credentials_cooldown_bail_threshold_secs:
                default_all_credentials_cooldown_bail_threshold_secs(),
            model_unavailable_breaker_enabled: true,
            import_disabled_by_default: true,
            balance_auto_refresh_secs: 0,
            balance_refresh_concurrency: default_balance_refresh_concurrency(),
            rate_limit_cooldown_min_secs: default_rate_limit_cooldown_min_secs(),
            rate_limit_cooldown_max_secs: default_rate_limit_cooldown_max_secs(),
            capacity_pressure_cooldown_secs: default_capacity_pressure_cooldown_secs(),
            rate_limit_ignore_retry_after: false,
            rate_limit_disable_cooldown: false,
            error_log_enabled: true,
            error_log_max_count: default_error_log_max_count(),
            error_log_max_age_days: default_error_log_max_age_days(),
            error_log_excluded_status_codes: Vec::new(),
            pricing: PricingConfig::default(),
            model_mapping: ModelMappingConfig::default(),
            models: Vec::new(),
            push_notification: PushNotificationConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    #[allow(dead_code)]
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在，返回默认配置
            return Ok(Self {
                config_path: Some(path.to_path_buf()),
                ..Default::default()
            });
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// 获取配置文件路径（如果有）
    #[allow(dead_code)]
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// 将当前配置写回原始配置文件
    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("配置文件路径未知，无法保存配置"))?;

        let content = serde_json::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(path, content)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults_enable_prompt_cache_accounting() {
        let config = Config::default();
        assert!(config.prompt_cache_accounting_enabled);
    }

    #[test]
    fn test_pricing_default_rules_match_and_cost() {
        let p = PricingConfig::default();
        // contains 匹配（默认规则均为 contains）
        let r = p.rate_for("claude-opus-4-6-thinking");
        assert_eq!(r.input, 15.0);
        assert_eq!(r.output, 75.0);
        // 1M 输入 + 1M 输出 = 15 + 75 = 90 美元
        let cost = p.cost_usd("claude-opus-4-6", 1_000_000, 1_000_000, 0, 0);
        assert!((cost - 90.0).abs() < 1e-9);
        // 缓存读写单价
        let cache_cost = p.cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 1_000_000);
        assert!((cache_cost - (0.3 + 3.75)).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_global_multiplier_scales_cost() {
        let mut p = PricingConfig::default();
        p.multiplier = 1.5;
        // 1M 输入 opus = 15，× 1.5 = 22.5
        let cost = p.cost_usd("claude-opus-4-6", 1_000_000, 0, 0, 0);
        assert!((cost - 22.5).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_unmatched_uses_default_zero() {
        let p = PricingConfig::default();
        // 未命中任何规则 → 默认价（全 0）
        assert_eq!(p.cost_usd("gpt-4o", 1_000_000, 1_000_000, 0, 0), 0.0);
    }

    #[test]
    fn test_pricing_match_types() {
        let rule = |mt: &str, pat: &str| PricingRule {
            label: String::new(),
            pattern: pat.to_string(),
            match_type: mt.to_string(),
            rate: PricingRate::default(),
        };
        assert!(rule("exact", "claude-opus-4-6").matches("claude-opus-4-6"));
        assert!(!rule("exact", "opus").matches("claude-opus-4-6"));
        assert!(rule("prefix", "claude-opus").matches("claude-opus-4-6"));
        assert!(rule("contains", "opus").matches("claude-opus-4-6"));
        assert!(rule("glob", "claude-*-4-6").matches("claude-opus-4-6"));
        assert!(rule("glob", "*opus*").matches("claude-opus-4-6"));
        assert!(!rule("glob", "claude-haiku-*").matches("claude-opus-4-6"));
        // 大小写不敏感
        assert!(rule("contains", "OPUS").matches("claude-opus-4-6"));
    }

    #[test]
    fn test_model_mapping_resolve() {
        let rule = |pat: &str, mt: &str, model: &str| ModelMappingRule {
            label: String::new(),
            pattern: pat.to_string(),
            match_type: mt.to_string(),
            model: model.to_string(),
        };

        // 空配置：无规则 → None（回退语义由 converter 决定）
        assert!(ModelMappingConfig::default().is_empty());
        assert!(
            ModelMappingConfig::default()
                .resolve("claude-opus-4-8")
                .is_none()
        );

        let cfg = ModelMappingConfig {
            rules: vec![
                rule("opus-4-8", "contains", "claude-opus-4.8"),
                rule("opus", "contains", "claude-opus-4.6"),
                rule("claude-sonnet-4-6", "exact", "claude-sonnet-4.6"),
            ],
        };
        // 自上而下第一条命中
        assert_eq!(
            cfg.resolve("claude-opus-4-8-thinking").as_deref(),
            Some("claude-opus-4.8")
        );
        assert_eq!(
            cfg.resolve("claude-opus-4-6").as_deref(),
            Some("claude-opus-4.6")
        );
        // exact 只精确匹配
        assert_eq!(
            cfg.resolve("claude-sonnet-4-6").as_deref(),
            Some("claude-sonnet-4.6")
        );
        assert!(cfg.resolve("claude-sonnet-4-6-thinking").is_none());
        // 未命中任何规则 → None（模型不存在）
        assert!(cfg.resolve("gpt-4").is_none());

        // 命中但目标为空串 → 视为未命中
        let cfg_empty_target = ModelMappingConfig {
            rules: vec![rule("haiku", "contains", "")],
        };
        assert!(cfg_empty_target.resolve("claude-haiku-4-5").is_none());
    }

    #[test]
    fn test_resolve_max_body_bytes_per_model() {
        let limit = |pat: &str, mt: &str, max_bytes: usize| ModelBodyLimitRule {
            label: String::new(),
            pattern: pat.to_string(),
            match_type: mt.to_string(),
            max_bytes,
        };

        let mut cfg = CompressionConfig::default();
        cfg.max_request_body_bytes = 4_718_592; // 全局默认

        // 未配置任何规则 → 一律回退全局值
        assert_eq!(cfg.resolve_max_body_bytes("claude-opus-4-8"), 4_718_592);

        cfg.per_model_body_limits = vec![
            limit("opus", "contains", 6_291_456),
            limit("haiku", "contains", 3_145_728),
        ];
        // 命中 opus / haiku 用各自上限（大小写不敏感）
        assert_eq!(
            cfg.resolve_max_body_bytes("claude-OPUS-4-8-thinking"),
            6_291_456
        );
        assert_eq!(cfg.resolve_max_body_bytes("claude-haiku-4-5"), 3_145_728);
        // 未命中（sonnet）→ 回退全局
        assert_eq!(cfg.resolve_max_body_bytes("claude-sonnet-4-6"), 4_718_592);

        // 自上而下第一条命中生效
        cfg.per_model_body_limits = vec![
            limit("opus", "contains", 1000),
            limit("opus-4-8", "contains", 2000),
        ];
        assert_eq!(cfg.resolve_max_body_bytes("claude-opus-4-8"), 1000);
    }

    #[test]
    fn test_config_deserializes_prompt_cache_accounting_false() {
        let config: Config = serde_json::from_str(r#"{"promptCacheAccountingEnabled":false}"#)
            .expect("config should deserialize");
        assert!(!config.prompt_cache_accounting_enabled);
    }
}
