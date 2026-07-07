//! 使用额度查询数据模型
//!
//! 包含 getUsageLimits API 的响应类型定义

use serde::Deserialize;

/// 使用额度查询响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 订阅信息
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,

    /// 使用量明细列表
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,

    /// 超额计费配置（与 setUserPreference 请求体同结构）
    #[serde(default)]
    pub overage_configuration: Option<OverageConfiguration>,

    /// 用户邮箱（仅当请求带 `isEmailRequired=true` 时上游返回；
    /// 接受 root 上的 `email` / `userEmail`，以及嵌套 `userInfo.email`）
    #[serde(default, alias = "email", alias = "userEmail")]
    pub user_email: Option<String>,

    /// 嵌套 userInfo（部分上游返回会把 email 放在这里）
    #[serde(default)]
    pub user_info: Option<UsageUserInfo>,
}

/// 嵌套 userInfo（容错读取多种字段名）
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageUserInfo {
    #[serde(default, alias = "userEmail")]
    pub email: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub user_id: Option<String>,
}

/// 超额计费配置
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    /// "ENABLED" / "DISABLED"
    #[serde(default)]
    pub overage_status: Option<String>,
}

/// 订阅信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    /// 订阅标题 (KIRO PRO+ / KIRO FREE 等)
    #[serde(default)]
    pub subscription_title: Option<String>,
}

/// 使用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdown {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 奖励额度列表（可能为 null）
    #[serde(default)]
    pub bonuses: Option<Vec<Bonus>>,

    /// 免费试用信息
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

/// 奖励额度
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: f64,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: f64,

    /// 状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub status: Option<String>,
}

impl Bonus {
    /// 检查 bonus 是否处于激活状态（大小写不敏感）
    pub fn is_active(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("ACTIVE"))
            .unwrap_or(false)
    }
}

/// 免费试用信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrialInfo {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 免费试用过期时间 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub free_trial_expiry: Option<f64>,

    /// 免费试用状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub free_trial_status: Option<String>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

// ============ 便捷方法实现 ============

impl FreeTrialInfo {
    /// 检查免费试用是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl UsageLimitsResponse {
    /// 获取订阅标题
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    /// 提取用户邮箱（容错从 root 字段或嵌套 userInfo 中获取）
    pub fn extract_email(&self) -> Option<String> {
        self.user_email
            .as_deref()
            .or_else(|| self.user_info.as_ref().and_then(|u| u.email.as_deref()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.contains('@'))
    }

    /// 获取 overage_status（"ENABLED" / "DISABLED"）
    pub fn overage_status(&self) -> Option<&str> {
        self.overage_configuration
            .as_ref()
            .and_then(|c| c.overage_status.as_deref())
    }

    /// 获取第一个使用量明细
    fn primary_breakdown(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 响应中是否带有可用的用量明细数据。
    ///
    /// 用于区分「上游返回 200 但 usageBreakdownList 为空/缺失」与「真实余额为零」：
    /// 前者 `usage_limit()`/`current_usage()` 都会回退到 0，若据此判定余额不足会误禁用凭据。
    pub fn has_usage_data(&self) -> bool {
        !self.usage_breakdown_list.is_empty()
    }

    /// 获取总使用限额（精确值）
    ///
    /// 累加基础额度、激活的免费试用额度和激活的奖励额度
    #[allow(clippy::collapsible_if)]
    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.usage_limit_with_precision;

        // 累加激活的 free trial 额度
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }

        // 累加激活的 bonus 额度
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.usage_limit;
                }
            }
        }

        total
    }

    /// 获取总当前使用量（精确值）
    ///
    /// 累加基础使用量、激活的免费试用使用量和激活的奖励使用量
    #[allow(clippy::collapsible_if)]
    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.current_usage_with_precision;

        // 累加激活的 free trial 使用量
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }

        // 累加激活的 bonus 使用量
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.current_usage;
                }
            }
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_usage_data_empty_response() {
        // 上游返回 200 但无 usageBreakdownList：has_usage_data 应为 false，
        // 且 usage_limit()/current_usage() 回退到 0（调用方据此跳过禁用判定）。
        let resp: UsageLimitsResponse = serde_json::from_str("{}").unwrap();
        assert!(!resp.has_usage_data());
        assert_eq!(resp.usage_limit(), 0.0);
        assert_eq!(resp.current_usage(), 0.0);
    }

    #[test]
    fn test_has_usage_data_with_breakdown() {
        let resp: UsageLimitsResponse = serde_json::from_str(
            r#"{"usageBreakdownList":[{"usageLimitWithPrecision":100.0,"currentUsageWithPrecision":30.0}]}"#,
        )
        .unwrap();
        assert!(resp.has_usage_data());
        assert_eq!(resp.usage_limit(), 100.0);
        assert_eq!(resp.current_usage(), 30.0);
    }
}
