//! 设备指纹生成器
//!

use sha2::{Digest, Sha256};

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

/// 标准化 machineId 格式
///
/// 支持以下格式：
/// - 64 字符十六进制字符串（直接返回）
/// - UUID 格式（如 "2582956e-cc88-4669-b546-07adbffcb894"，移除连字符后补齐到 64 字符）
fn normalize_machine_id(machine_id: &str) -> Option<String> {
    let trimmed = machine_id.trim();

    // 如果已经是 64 字符，直接返回
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_string());
    }

    // 尝试解析 UUID 格式（移除连字符）
    let without_dashes: String = trimmed.chars().filter(|c| *c != '-').collect();

    // UUID 去掉连字符后是 32 字符
    if without_dashes.len() == 32 && without_dashes.chars().all(|c| c.is_ascii_hexdigit()) {
        // 补齐到 64 字符（重复一次）
        return Some(format!("{}{}", without_dashes, without_dashes));
    }

    // 无法识别的格式
    None
}

/// 根据凭证信息生成唯一的 Machine ID
///
/// 优先使用凭据级 machineId，其次使用 config.machineId，然后使用 refreshToken 或 API Key 生成
pub fn generate_from_credentials(credentials: &KiroCredentials, config: &Config) -> Option<String> {
    // 如果配置了凭据级 machineId，优先使用
    if let Some(ref machine_id) = credentials.machine_id
        && let Some(normalized) = normalize_machine_id(machine_id)
    {
        return Some(normalized);
    }

    // 如果配置了全局 machineId，作为默认值
    if let Some(ref machine_id) = config.machine_id
        && let Some(normalized) = normalize_machine_id(machine_id)
    {
        return Some(normalized);
    }

    // 从「不随 token 刷新而变化」的稳定账号标识派生，保证同一账号的 machineId 长期稳定。
    // 真实 Kiro 客户端的 machineId 是设备级、跨会话不变的；若用会轮换的 refresh_token 派生
    // （IdC/Social 刷新约每小时轮换一次），machineId 就会随之改变，形成「同一设备每小时换机器码」
    // 的强异常指纹。优先级：IdC client_id > email > 凭据自增 id。
    let stable_seed = credentials
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            credentials
                .email
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .or_else(|| credentials.id.map(|id| format!("cred-{id}")));
    if let Some(seed) = stable_seed {
        return Some(sha256_hex(&format!("KiroDevice/{seed}")));
    }

    // API Key 不会轮换，可稳定派生
    if let Some(ref api_key) = credentials.kiro_api_key
        && !api_key.is_empty()
    {
        return Some(sha256_hex(&format!("KiroAPIKey/{}", api_key)));
    }

    // 最后兜底：只有 refresh_token 可用（无 id/email/client_id 的临时凭据，罕见）。
    // 注意 refresh_token 会轮换，此路径下 machineId 会随刷新变化，仅作降级。
    if let Some(ref refresh_token) = credentials.refresh_token
        && !refresh_token.is_empty()
    {
        return Some(sha256_hex(&format!("KotlinNativeAPI/{}", refresh_token)));
    }

    // 没有有效的凭证
    None
}

/// SHA256 哈希实现（返回十六进制字符串）
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(result.len(), 64);
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn test_generate_with_custom_machine_id() {
        let credentials = KiroCredentials::default();
        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, Some("a".repeat(64)));
    }

    #[test]
    fn test_generate_with_credential_machine_id_overrides_config() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("b".repeat(64));

        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, Some("b".repeat(64)));
    }

    #[test]
    fn test_machine_id_stable_across_refresh_token_rotation() {
        let config = Config::default();
        // 同一账号（id 相同），刷新后 refresh_token 轮换
        let mut c1 = KiroCredentials::default();
        c1.id = Some(7);
        c1.refresh_token = Some("refresh-token-A".to_string());
        let mut c2 = KiroCredentials::default();
        c2.id = Some(7);
        c2.refresh_token = Some("refresh-token-B-rotated".to_string());
        // machineId 不应随 refresh_token 轮换而变化
        assert_eq!(
            generate_from_credentials(&c1, &config),
            generate_from_credentials(&c2, &config)
        );

        // 不同账号（id 不同）machineId 应不同
        let mut c3 = KiroCredentials::default();
        c3.id = Some(8);
        c3.refresh_token = Some("refresh-token-A".to_string());
        assert_ne!(
            generate_from_credentials(&c1, &config),
            generate_from_credentials(&c3, &config)
        );

        // client_id（IdC 稳定标识）优先于 id，且不随 refresh_token 变化
        let mut idc1 = KiroCredentials::default();
        idc1.id = Some(7);
        idc1.client_id = Some("oidc-client-xyz".to_string());
        idc1.refresh_token = Some("rt-1".to_string());
        let mut idc2 = KiroCredentials::default();
        idc2.id = Some(99); // 即便 id 不同，只要 client_id 相同就应稳定
        idc2.client_id = Some("oidc-client-xyz".to_string());
        idc2.refresh_token = Some("rt-2".to_string());
        assert_eq!(
            generate_from_credentials(&idc1, &config),
            generate_from_credentials(&idc2, &config)
        );
    }

    #[test]
    fn test_generate_with_refresh_token() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("test_refresh_token".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn test_generate_with_api_key() {
        let mut credentials = KiroCredentials::default();
        credentials.kiro_api_key = Some("ksk_test_api_key".to_string());
        credentials.auth_method = Some("api_key".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn test_generate_without_credentials() {
        let credentials = KiroCredentials::default();
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_normalize_uuid_format() {
        // UUID 格式应该被转换为 64 字符
        let uuid = "2582956e-cc88-4669-b546-07adbffcb894";
        let result = normalize_machine_id(uuid);
        assert!(result.is_some());
        let normalized = result.unwrap();
        assert_eq!(normalized.len(), 64);
        // UUID 去掉连字符后重复一次
        assert_eq!(
            normalized,
            "2582956ecc884669b54607adbffcb8942582956ecc884669b54607adbffcb894"
        );
    }

    #[test]
    fn test_normalize_64_char_hex() {
        // 64 字符十六进制应该直接返回
        let hex64 = "a".repeat(64);
        let result = normalize_machine_id(&hex64);
        assert_eq!(result, Some(hex64));
    }

    #[test]
    fn test_normalize_invalid_format() {
        // 无效格式应该返回 None
        assert!(normalize_machine_id("invalid").is_none());
        assert!(normalize_machine_id("too-short").is_none());
        assert!(normalize_machine_id(&"g".repeat(64)).is_none()); // 非十六进制
    }

    #[test]
    fn test_generate_with_uuid_machine_id() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("2582956e-cc88-4669-b546-07adbffcb894".to_string());

        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().len(), 64);
    }
}
