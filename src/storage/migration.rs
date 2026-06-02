//! Schema 创建（CREATE TABLE IF NOT EXISTS） + 启动期 JSON → SQLite 一次性导入

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS credentials (
    id                  INTEGER PRIMARY KEY,
    access_token        TEXT,
    refresh_token       TEXT,
    kiro_api_key        TEXT,
    profile_arn         TEXT,
    expires_at          TEXT,
    auth_method         TEXT,
    client_id           TEXT,
    client_secret       TEXT,
    priority            INTEGER NOT NULL DEFAULT 0,
    region              TEXT,
    api_region          TEXT,
    machine_id          TEXT,
    endpoint            TEXT,
    email               TEXT,
    subscription_title  TEXT,
    proxy_slot_id       TEXT,
    disabled            INTEGER NOT NULL DEFAULT 0,
    allow_overuse       INTEGER NOT NULL DEFAULT 0,
    rpm                 INTEGER,
    last_overage_status TEXT
);

CREATE INDEX IF NOT EXISTS idx_credentials_priority ON credentials(priority);

CREATE TABLE IF NOT EXISTS proxies (
    id              TEXT PRIMARY KEY,
    url             TEXT NOT NULL,
    username        TEXT,
    password        TEXT,
    expires_at      TEXT NOT NULL,
    slots           INTEGER NOT NULL DEFAULT 1,
    label           TEXT,
    created_at      TEXT NOT NULL,
    last_rotated_at TEXT
);

CREATE TABLE IF NOT EXISTS proxy_bindings (
    proxy_id        TEXT NOT NULL,
    credential_id   INTEGER NOT NULL,
    PRIMARY KEY (proxy_id, credential_id),
    FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_proxy_bindings_credential ON proxy_bindings(credential_id);

CREATE TABLE IF NOT EXISTS balance_cache (
    credential_id       INTEGER PRIMARY KEY,
    remaining           REAL NOT NULL,
    usage_limit         REAL NOT NULL,
    usage_percentage    REAL NOT NULL,
    subscription_title  TEXT,
    cached_at           REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS rpm_history (
    credential_id   INTEGER NOT NULL,
    minute_ts       INTEGER NOT NULL,
    count           INTEGER NOT NULL,
    rl_count        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (credential_id, minute_ts)
);

CREATE INDEX IF NOT EXISTS idx_rpm_history_minute ON rpm_history(minute_ts);

CREATE TABLE IF NOT EXISTS api_keys (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    key                 TEXT NOT NULL UNIQUE,
    name                TEXT NOT NULL,
    description         TEXT,
    enabled             INTEGER NOT NULL DEFAULT 1,
    max_concurrent      INTEGER NOT NULL DEFAULT 0,
    cache_read_min_pct  INTEGER NOT NULL DEFAULT 0,
    cache_read_max_pct  INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    last_used_at        TEXT,
    success_count       INTEGER NOT NULL DEFAULT 0,
    fail_count          INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_key ON api_keys(key);

CREATE TABLE IF NOT EXISTS error_logs (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    at                TEXT NOT NULL,
    credential_id     INTEGER,
    endpoint          TEXT,
    status_code       INTEGER NOT NULL,
    upstream_status   INTEGER,
    error_kind        TEXT NOT NULL,
    model             TEXT,
    summary           TEXT NOT NULL,
    request_method    TEXT,
    request_path      TEXT,
    request_headers   TEXT,
    response_headers  TEXT,
    request_body      TEXT,
    response_body     TEXT,
    user_id           TEXT,
    request_id        TEXT
);

CREATE INDEX IF NOT EXISTS idx_error_logs_at ON error_logs(at DESC);
CREATE INDEX IF NOT EXISTS idx_error_logs_credential ON error_logs(credential_id);
CREATE INDEX IF NOT EXISTS idx_error_logs_status ON error_logs(status_code);
CREATE INDEX IF NOT EXISTS idx_error_logs_kind ON error_logs(error_kind);
"#;

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL).context("初始化 SQLite schema 失败")?;
    add_column_if_missing(conn, "credentials", "allow_overuse", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "credentials", "rpm", "INTEGER")?;
    add_column_if_missing(conn, "credentials", "last_overage_status", "TEXT")?;
    // 凭据自动禁用事件落 error_logs 时记录禁用原因（AccountSuspended/AuthenticationFailed 等）
    add_column_if_missing(conn, "error_logs", "disable_reason", "TEXT")?;
    // 每分钟 429 增量（用于「最佳 RPM」分析）；老库默认 0
    add_column_if_missing(conn, "rpm_history", "rl_count", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

/// 幂等地为已有表追加列：通过 PRAGMA table_info 探测列是否存在，不存在则 ALTER TABLE。
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    column_def: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let mut rows = stmt.query([])?;
    let mut exists = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            exists = true;
            break;
        }
    }
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, column_def),
            [],
        )
        .with_context(|| format!("ALTER TABLE {} ADD COLUMN {} 失败", table, column))?;
    }
    Ok(())
}

/// 把现有 JSON 文件一次性迁移到 SQLite，并将 JSON 改名为 .migrated。
///
/// 仅在数据库的 credentials 表为空时执行（防止启动重跑覆盖现有数据）。
pub fn migrate_json_if_needed(
    store: &super::Store,
    credentials_path: Option<&Path>,
    proxies_path: Option<&Path>,
    balance_cache_path: Option<&Path>,
) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();
    let conn = store.conn()?;
    let credential_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM credentials", [], |r| r.get(0))?;
    if credential_count > 0 {
        // DB 已有数据，跳过迁移
        report.skipped = true;
        return Ok(report);
    }
    drop(conn);

    // credentials.json
    if let Some(p) = credentials_path
        && p.exists()
    {
        match std::fs::read_to_string(p) {
            Ok(content) if !content.trim().is_empty() => {
                use crate::kiro::model::credentials::CredentialsConfig;
                if let Ok(cfg) = serde_json::from_str::<CredentialsConfig>(&content) {
                    let creds = cfg.into_sorted_credentials();
                    let imported = creds.len();
                    if imported > 0 {
                        store.replace_all_credentials(&creds)?;
                        report.credentials_imported = imported;
                        let migrated = p.with_extension("json.migrated");
                        let _ = std::fs::rename(p, migrated);
                    }
                } else {
                    tracing::warn!("迁移：credentials.json 解析失败，跳过");
                }
            }
            _ => {}
        }
    }

    // proxies.json
    if let Some(p) = proxies_path
        && p.exists()
    {
        match std::fs::read_to_string(p) {
            Ok(content) if !content.trim().is_empty() => {
                use crate::kiro::proxy_pool::ProxyEntry;
                if let Ok(entries) = serde_json::from_str::<Vec<ProxyEntry>>(&content) {
                    let imported = entries.len();
                    if imported > 0 {
                        store.replace_all_proxies(&entries)?;
                        report.proxies_imported = imported;
                        let migrated = p.with_extension("json.migrated");
                        let _ = std::fs::rename(p, migrated);
                    }
                } else {
                    tracing::warn!("迁移：proxies.json 解析失败，跳过");
                }
            }
            _ => {}
        }
    }

    // balance_cache.json
    if let Some(p) = balance_cache_path
        && p.exists()
    {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LegacyBalanceData {
            #[serde(default)]
            remaining: f64,
            #[serde(default)]
            usage_limit: f64,
            #[serde(default)]
            usage_percentage: f64,
            subscription_title: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct LegacyBalance {
            cached_at: f64,
            data: LegacyBalanceData,
        }
        if let Ok(content) = std::fs::read_to_string(p)
            && !content.trim().is_empty()
            && let Ok(map) =
                serde_json::from_str::<std::collections::HashMap<String, LegacyBalance>>(&content)
        {
            for (k, v) in map {
                if let Ok(id) = k.parse::<u64>() {
                    let row = super::BalanceCacheRow {
                        remaining: v.data.remaining,
                        usage_limit: v.data.usage_limit,
                        usage_percentage: v.data.usage_percentage,
                        subscription_title: v.data.subscription_title,
                        cached_at: v.cached_at,
                    };
                    store.upsert_balance_cache(id, &row)?;
                    report.balances_imported += 1;
                }
            }
            let migrated = p.with_extension("json.migrated");
            let _ = std::fs::rename(p, migrated);
        }
    }

    Ok(report)
}

#[derive(Debug, Default)]
pub struct MigrationReport {
    pub skipped: bool,
    pub credentials_imported: usize,
    pub proxies_imported: usize,
    pub balances_imported: usize,
}
