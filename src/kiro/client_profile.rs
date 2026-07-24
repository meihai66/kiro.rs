//! 客户端桌面指纹（平台/版本三元组）派生
//!
//! 移植自 Kiro-Go：按凭据稳定种子从加权池确定性派生
//! `{system_version, kiro_version, node_version}`，保证同一账号跨请求、跨重启稳定，
//! 且平台池只含真实存在的 macOS / Windows 构建号，**永不输出 Linux**——真实 Kiro IDE
//! 客户端不会在 User-Agent 里报告 Linux 服务器内核，出现即为最强的「非真实用户」信号。
//!
//! 相比旧的 `config.system_version` 全局随机单值，本模块解决两个指纹异常：
//! 1. 所有凭据共用同一平台（像「一批机器同一套镜像」）；
//! 2. 该值进程启动时随机决定，重启后可能在 darwin↔win32 间跳变（同一账号「换了操作系统」）。

use sha2::{Digest, Sha256};

use crate::model::config::Config;

/// 客户端桌面指纹三元组
#[derive(Debug, Clone)]
pub struct ClientProfile {
    pub system_version: String,
    pub kiro_version: String,
    pub node_version: String,
}

/// 加权指纹池：`(system_version, kiro_version, node_version, weight)`
///
/// 设计约束（移植自 Kiro-Go）：
/// - macOS 为主、少量 Windows，贴近真实账号来源分布（分布错了会制造「人群不真实」的新信号）；
/// - **绝不含 Linux**；
/// - 每个 `system_version` 均为真实存在的 macOS(Darwin) / Windows 构建号；
/// - `kiro_version` / `node_version` 锁定单一组合——真实用户会自动更新，版本高度统一才正常，
///   只在平台维度做多样化。
const CLIENT_PROFILE_POOL: &[(&str, &str, &str, u32)] = &[
    ("darwin#24.6.0", "0.11.107", "22.22.0", 35), // macOS 15.6 Sequoia
    ("darwin#24.5.0", "0.11.107", "22.22.0", 25), // macOS 15.5 Sequoia
    ("darwin#23.6.0", "0.11.107", "22.22.0", 25), // macOS 14.6 Sonoma
    ("win32#10.0.22631", "0.11.107", "22.22.0", 10), // Win11 23H2
    ("win32#10.0.19045", "0.11.107", "22.22.0", 5), // Win10 22H2
];

/// 兜底指纹：池被误配为空时也绝不回退到 `runtime.GOOS`（即绝不 Linux）。
const FALLBACK_PROFILE: (&str, &str, &str) = ("darwin#24.6.0", "0.11.107", "22.22.0");

fn fallback_profile() -> ClientProfile {
    let (system_version, kiro_version, node_version) = FALLBACK_PROFILE;
    ClientProfile {
        system_version: system_version.to_string(),
        kiro_version: kiro_version.to_string(),
        node_version: node_version.to_string(),
    }
}

fn profile_from_entry(entry: &(&str, &str, &str, u32)) -> ClientProfile {
    ClientProfile {
        system_version: entry.0.to_string(),
        kiro_version: entry.1.to_string(),
        node_version: entry.2.to_string(),
    }
}

/// 按种子确定性派生桌面指纹（纯函数）。
///
/// 用 `sha256("KiroProfile/" + seed)` 前 8 字节作为 u64 选择器，按累计权重选池。
/// 前缀与 `machine_id` 的 `KiroDevice/` 不同，使平台选择独立于设备 id 哈希，
/// 但两者都稳定绑定同一账号（种子通常传已派生好的 machineId）。
pub fn derive_from_seed(seed: &str) -> ClientProfile {
    let total_weight: u32 = CLIENT_PROFILE_POOL.iter().map(|p| p.3).sum();
    if total_weight == 0 || CLIENT_PROFILE_POOL.is_empty() {
        return fallback_profile();
    }

    let mut hasher = Sha256::new();
    hasher.update(format!("KiroProfile/{seed}").as_bytes());
    let sum = hasher.finalize();

    let mut selector: u64 = 0;
    for byte in sum.iter().take(8) {
        selector = (selector << 8) | u64::from(*byte);
    }
    let target = (selector % u64::from(total_weight)) as u32;

    let mut cumulative = 0u32;
    for entry in CLIENT_PROFILE_POOL {
        cumulative += entry.3;
        if target < cumulative {
            return profile_from_entry(entry);
        }
    }
    // 不可达（累计权重已覆盖全域），防御性兜底。
    profile_from_entry(&CLIENT_PROFILE_POOL[CLIENT_PROFILE_POOL.len() - 1])
}

/// 解析最终指纹：先按种子派生，再用 `config` 里显式配置的字段逐项覆盖。
///
/// 保留 operator 手动覆盖能力：`config.kiro_version` / `system_version` / `node_version`
/// 显式非空时优先生效，否则使用派生值。
pub fn resolve(seed: &str, config: &Config) -> ClientProfile {
    let mut profile = derive_from_seed(seed);
    if let Some(value) = non_empty(config.kiro_version.as_deref()) {
        profile.kiro_version = value;
    }
    if let Some(value) = non_empty(config.system_version.as_deref()) {
        profile.system_version = value;
    }
    if let Some(value) = non_empty(config.node_version.as_deref()) {
        profile.node_version = value;
    }
    profile
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_is_deterministic() {
        let a = derive_from_seed("seed-abc");
        let b = derive_from_seed("seed-abc");
        assert_eq!(a.system_version, b.system_version);
        assert_eq!(a.kiro_version, b.kiro_version);
        assert_eq!(a.node_version, b.node_version);
    }

    #[test]
    fn test_never_returns_linux() {
        // 无论种子如何，平台永远是 darwin / win32，绝不 Linux
        for i in 0..2000 {
            let profile = derive_from_seed(&format!("seed-{i}"));
            assert!(
                profile.system_version.starts_with("darwin#")
                    || profile.system_version.starts_with("win32#"),
                "unexpected platform: {}",
                profile.system_version
            );
        }
    }

    #[test]
    fn test_distribution_is_mac_weighted() {
        // 采样应呈 mac 为主的分布（池权重 mac=85 / win=15）
        let mut mac = 0;
        let mut win = 0;
        for i in 0..5000 {
            let profile = derive_from_seed(&format!("acct-{i}"));
            if profile.system_version.starts_with("darwin#") {
                mac += 1;
            } else {
                win += 1;
            }
        }
        assert!(
            mac > win * 3,
            "expected mac-weighted, got mac={mac} win={win}"
        );
    }

    #[test]
    fn test_config_override_takes_precedence() {
        let mut config = Config::default();
        config.system_version = Some("win32#10.0.99999".to_string());
        config.kiro_version = Some("9.9.9".to_string());
        config.node_version = Some("18.0.0".to_string());

        let profile = resolve("any-seed", &config);
        assert_eq!(profile.system_version, "win32#10.0.99999");
        assert_eq!(profile.kiro_version, "9.9.9");
        assert_eq!(profile.node_version, "18.0.0");
    }

    #[test]
    fn test_resolve_uses_derived_when_config_empty() {
        let config = Config::default();
        let derived = derive_from_seed("stable-seed");
        let resolved = resolve("stable-seed", &config);
        assert_eq!(resolved.system_version, derived.system_version);
        assert_eq!(resolved.kiro_version, derived.kiro_version);
        assert_eq!(resolved.node_version, derived.node_version);
    }
}
