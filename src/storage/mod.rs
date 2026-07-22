//! SQLite 持久化层
//!
//! - WAL 模式 + 连接池（多读单写）
//! - 启动期建表 + 从既有 JSON 文件一次性导入
//! - 同步 API；调用方负责必要时 `spawn_blocking`（SQLite 操作通常 ms 级，可直接 await）
//!
//! 表结构概览：
//! - credentials              凭据主表（与 KiroCredentials 对应）
//! - proxies                  代理池主表
//! - proxy_bindings           proxy_id ↔ credential_id 多对多
//! - balance_cache            余额缓存
//! - rpm_history              每分钟 RPM 历史采样（用于趋势图）

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::proxy_pool::{ProxyDisabledCategory, ProxyEntry};

pub mod migration;

pub type Conn = r2d2::PooledConnection<SqliteConnectionManager>;
pub type SqlitePool = Pool<SqliteConnectionManager>;

/// SQLite 持久化存储入口
#[allow(dead_code)]
pub struct Store {
    pool: SqlitePool,
    db_path: PathBuf,
}

#[allow(dead_code)]
impl Store {
    /// 打开（必要时创建）数据库文件，开 WAL 模式，建表
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Arc<Self>> {
        let db_path = path.as_ref().to_path_buf();
        let manager = SqliteConnectionManager::file(&db_path).with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode = WAL;\n\
                 PRAGMA synchronous = NORMAL;\n\
                 PRAGMA foreign_keys = ON;\n\
                 PRAGMA busy_timeout = 5000;",
            )
        });
        let pool = Pool::builder()
            .max_size(8)
            .build(manager)
            .with_context(|| format!("创建 SQLite 连接池失败: {}", db_path.display()))?;

        // 建表
        {
            let conn = pool.get()?;
            migration::ensure_schema(&conn)?;
        }

        // 库内明文存放凭据/token，收紧文件权限为仅属主可读写（0600），纵深防御防同机其他
        // 本地用户读取。WAL/SHM 会镜像库内容，一并收紧。best-effort：失败不阻断启动。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                let p = PathBuf::from(format!("{}{}", db_path.display(), suffix));
                if p.exists()
                    && let Err(e) =
                        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
                {
                    tracing::warn!("收紧数据库文件权限失败 {}: {}", p.display(), e);
                }
            }
        }

        Ok(Arc::new(Self { pool, db_path }))
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn conn(&self) -> Result<Conn> {
        Ok(self.pool.get()?)
    }

    // ============ Credentials ============

    /// 列出所有凭据（按 priority 升序）
    pub fn list_credentials(&self) -> Result<Vec<KiroCredentials>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, access_token, refresh_token, kiro_api_key, profile_arn, expires_at, \
             auth_method, client_id, client_secret, priority, region, api_region, machine_id, \
             endpoint, email, subscription_title, proxy_slot_id, disabled, allow_overuse, rpm, \
             last_overage_status \
             FROM credentials ORDER BY priority ASC, id ASC",
        )?;
        let rows = stmt.query_map([], row_to_credentials)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// 一次性写入全部凭据（清空 + 批量 insert）；用于从 JSON 迁移或全量同步
    pub fn replace_all_credentials(&self, creds: &[KiroCredentials]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM credentials", [])?;
        for c in creds {
            insert_credential(&tx, c)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 更新或插入单个凭据（按 id）
    pub fn upsert_credential(&self, c: &KiroCredentials) -> Result<()> {
        let conn = self.conn()?;
        upsert_credential_inner(&conn, c)
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM credentials WHERE id = ?1", params![id as i64])?;
        Ok(())
    }

    // ============ Proxies ============

    pub fn list_proxies(&self) -> Result<Vec<ProxyEntry>> {
        let conn = self.conn()?;
        let mut entries: Vec<ProxyEntry> = {
            let mut stmt = conn.prepare(
                "SELECT id, url, username, password, expires_at, slots, label, created_at, last_rotated_at, \
                 disabled, disabled_category, disabled_reason \
                 FROM proxies",
            )?;
            stmt.query_map([], row_to_proxy_entry)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        // 加载 bindings
        let mut bind_stmt = conn.prepare("SELECT proxy_id, credential_id FROM proxy_bindings")?;
        let binds: Vec<(String, u64)> = bind_stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (pid, cred_id) in binds {
            if let Some(e) = entries.iter_mut().find(|e| e.id == pid) {
                e.bound_credential_ids.push(cred_id);
            }
        }
        Ok(entries)
    }

    /// 全量替换代理池
    pub fn replace_all_proxies(&self, entries: &[ProxyEntry]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM proxy_bindings", [])?;
        tx.execute("DELETE FROM proxies", [])?;
        for e in entries {
            tx.execute(
                "INSERT INTO proxies(id, url, username, password, expires_at, slots, label, created_at, last_rotated_at, disabled, disabled_category, disabled_reason) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    e.id,
                    e.url,
                    e.username,
                    e.password,
                    e.expires_at.to_rfc3339(),
                    e.slots as i64,
                    e.label,
                    e.created_at.to_rfc3339(),
                    e.last_rotated_at.map(|d| d.to_rfc3339()),
                    e.disabled as i64,
                    e.disabled_category.map(|c| c.as_str().to_string()),
                    e.disabled_reason,
                ],
            )?;
            for cred_id in &e.bound_credential_ids {
                tx.execute(
                    "INSERT INTO proxy_bindings(proxy_id, credential_id) VALUES (?1, ?2)",
                    params![e.id, *cred_id as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ============ Balance cache ============

    pub fn get_balance_cache(&self) -> Result<Vec<(u64, BalanceCacheRow)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT credential_id, remaining, usage_limit, usage_percentage, subscription_title, cached_at \
             FROM balance_cache",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                BalanceCacheRow {
                    remaining: r.get(1)?,
                    usage_limit: r.get(2)?,
                    usage_percentage: r.get(3)?,
                    subscription_title: r.get(4)?,
                    cached_at: r.get(5)?,
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_balance_cache(&self, cred_id: u64, row: &BalanceCacheRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO balance_cache(credential_id, remaining, usage_limit, usage_percentage, subscription_title, cached_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(credential_id) DO UPDATE SET \
                remaining=excluded.remaining, usage_limit=excluded.usage_limit, \
                usage_percentage=excluded.usage_percentage, subscription_title=excluded.subscription_title, \
                cached_at=excluded.cached_at",
            params![
                cred_id as i64,
                row.remaining,
                row.usage_limit,
                row.usage_percentage,
                row.subscription_title,
                row.cached_at,
            ],
        )?;
        Ok(())
    }

    pub fn delete_balance_cache(&self, cred_id: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM balance_cache WHERE credential_id = ?1",
            params![cred_id as i64],
        )?;
        Ok(())
    }

    // ============ RPM history ============

    /// 写一个分钟的 RPM 数据点（count = 该分钟末 60s 窗口 RPM，rl_count = 该分钟新增 429 数）
    pub fn record_rpm(
        &self,
        cred_id: u64,
        minute_ts: i64,
        count: u32,
        rl_count: u32,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO rpm_history(credential_id, minute_ts, count, rl_count) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(credential_id, minute_ts) DO UPDATE SET count=excluded.count, rl_count=excluded.rl_count",
            params![cred_id as i64, minute_ts, count as i64, rl_count as i64],
        )?;
        Ok(())
    }

    /// 取过去 hours 小时全部凭据的 (credential_id, rpm, rl_count) 原始数据点。
    /// 用于「最佳 RPM」分桶分析：按 credential_id 分组后由上层做分桶。
    pub fn rpm_analysis_all(&self, hours: i64) -> Result<Vec<(u64, u32, u32)>> {
        let cutoff_minute = (Utc::now().timestamp() - hours * 3600) / 60;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT credential_id, count, rl_count FROM rpm_history \
             WHERE minute_ts >= ?1 ORDER BY credential_id ASC",
        )?;
        let rows = stmt.query_map(params![cutoff_minute], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, i64>(2)? as u32,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// 取过去 hours 小时的所有凭据汇总 RPM 历史（按分钟）
    pub fn rpm_history_aggregate(&self, hours: i64) -> Result<Vec<(i64, u32)>> {
        let cutoff = Utc::now().timestamp() - hours * 3600;
        let cutoff_minute = cutoff / 60;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT minute_ts, SUM(count) FROM rpm_history \
             WHERE minute_ts >= ?1 \
             GROUP BY minute_ts ORDER BY minute_ts ASC",
        )?;
        let rows = stmt.query_map(params![cutoff_minute], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// 取过去 hours 小时的每分钟 RPM 数据
    pub fn rpm_history(&self, cred_id: u64, hours: i64) -> Result<Vec<(i64, u32)>> {
        let cutoff = Utc::now().timestamp() - hours * 3600;
        let cutoff_minute = cutoff / 60;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT minute_ts, count FROM rpm_history \
             WHERE credential_id = ?1 AND minute_ts >= ?2 \
             ORDER BY minute_ts ASC",
        )?;
        let rows = stmt.query_map(params![cred_id as i64, cutoff_minute], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// 清理超过 N 天的 RPM 历史（默认 7 天）
    pub fn purge_old_rpm(&self, days: i64) -> Result<usize> {
        let cutoff_minute = (Utc::now().timestamp() - days * 86400) / 60;
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM rpm_history WHERE minute_ts < ?1",
            params![cutoff_minute],
        )?;
        Ok(n)
    }
}

// ============ API Keys ============

#[derive(Debug, Clone)]
pub struct ApiKeyRow {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub max_concurrent: u32,
    pub cache_read_min_pct: u32,
    pub cache_read_max_pct: u32,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub success_count: u64,
    pub fail_count: u64,
    /// 允许使用的凭据 ID 列表（空 = 全部可用）
    pub allowed_credentials: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct ApiKeyCreate {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub max_concurrent: u32,
    pub cache_read_min_pct: u32,
    pub cache_read_max_pct: u32,
    /// 允许使用的凭据 ID 列表（空 = 全部可用）
    pub allowed_credentials: Vec<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ApiKeyUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub max_concurrent: Option<u32>,
    pub cache_read_min_pct: Option<u32>,
    pub cache_read_max_pct: Option<u32>,
    /// 允许使用的凭据范围（Some(空) = 恢复为全部可用；None = 不修改）
    pub allowed_credentials: Option<Vec<u64>>,
}

/// 把凭据 ID 列表编码为 CSV（去重、升序）；空列表编码为空串（= 全部可用）
fn encode_credential_ids(ids: &[u64]) -> String {
    let mut v: Vec<u64> = ids.to_vec();
    v.sort_unstable();
    v.dedup();
    v.iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// 解析 CSV 凭据 ID（忽略空白与非法项）
fn decode_credential_ids(s: Option<&str>) -> Vec<u64> {
    let Some(s) = s else {
        return Vec::new();
    };
    s.split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .collect()
}

impl Store {
    pub fn list_api_keys(&self) -> Result<Vec<ApiKeyRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, key, name, description, enabled, max_concurrent, \
             cache_read_min_pct, cache_read_max_pct, created_at, last_used_at, \
             success_count, fail_count, allowed_credentials FROM api_keys ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let created: String = r.get(8)?;
            let last_used: Option<String> = r.get(9)?;
            let allowed: Option<String> = r.get(12)?;
            Ok(ApiKeyRow {
                id: r.get(0)?,
                key: r.get(1)?,
                name: r.get(2)?,
                description: r.get(3)?,
                enabled: r.get::<_, i64>(4)? != 0,
                max_concurrent: r.get::<_, i64>(5)? as u32,
                cache_read_min_pct: r.get::<_, i64>(6)? as u32,
                cache_read_max_pct: r.get::<_, i64>(7)? as u32,
                created_at: parse_dt(&created).unwrap_or_else(Utc::now),
                last_used_at: last_used.as_deref().and_then(parse_dt),
                success_count: r.get::<_, i64>(10)? as u64,
                fail_count: r.get::<_, i64>(11)? as u64,
                allowed_credentials: decode_credential_ids(allowed.as_deref()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn create_api_key(&self, c: &ApiKeyCreate) -> Result<ApiKeyRow> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO api_keys(key, name, description, enabled, max_concurrent, \
             cache_read_min_pct, cache_read_max_pct, created_at, allowed_credentials) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                c.key,
                c.name,
                c.description,
                if c.enabled { 1 } else { 0 } as i64,
                c.max_concurrent as i64,
                c.cache_read_min_pct as i64,
                c.cache_read_max_pct as i64,
                now,
                encode_credential_ids(&c.allowed_credentials),
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(ApiKeyRow {
            id,
            key: c.key.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            enabled: c.enabled,
            max_concurrent: c.max_concurrent,
            cache_read_min_pct: c.cache_read_min_pct,
            cache_read_max_pct: c.cache_read_max_pct,
            created_at: Utc::now(),
            last_used_at: None,
            success_count: 0,
            fail_count: 0,
            allowed_credentials: {
                let mut v = c.allowed_credentials.clone();
                v.sort_unstable();
                v.dedup();
                v
            },
        })
    }

    pub fn update_api_key(&self, id: i64, u: &ApiKeyUpdate) -> Result<()> {
        let conn = self.conn()?;
        // 简单做：动态拼 SET 子句
        let mut sets: Vec<&str> = Vec::new();
        let mut p: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(name) = &u.name {
            sets.push("name = ?");
            p.push(name.clone().into());
        }
        if let Some(desc_opt) = &u.description {
            sets.push("description = ?");
            p.push(desc_opt.clone().into());
        }
        if let Some(en) = u.enabled {
            sets.push("enabled = ?");
            p.push((if en { 1i64 } else { 0i64 }).into());
        }
        if let Some(mc) = u.max_concurrent {
            sets.push("max_concurrent = ?");
            p.push((mc as i64).into());
        }
        if let Some(v) = u.cache_read_min_pct {
            sets.push("cache_read_min_pct = ?");
            p.push((v as i64).into());
        }
        if let Some(v) = u.cache_read_max_pct {
            sets.push("cache_read_max_pct = ?");
            p.push((v as i64).into());
        }
        if let Some(ids) = &u.allowed_credentials {
            sets.push("allowed_credentials = ?");
            p.push(encode_credential_ids(ids).into());
        }
        if sets.is_empty() {
            return Ok(());
        }
        p.push(id.into());
        let sql = format!("UPDATE api_keys SET {} WHERE id = ?", sets.join(", "));
        conn.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
        Ok(())
    }

    pub fn delete_api_key(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM api_keys WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 全局总请求计数（success+fail 在 api_keys 表的 SUM）
    pub fn aggregate_request_counts(&self) -> Result<(u64, u64)> {
        let conn = self.conn()?;
        let row: (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(success_count), 0), COALESCE(SUM(fail_count), 0) FROM api_keys",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((row.0 as u64, row.1 as u64))
    }

    /// 重置所有 api_keys 的 success_count / fail_count。
    /// 用于"清空统计"按钮——不动 RPM 历史和错误日志。
    pub fn reset_all_request_counts(&self) -> Result<u64> {
        let conn = self.conn()?;
        let n = conn.execute("UPDATE api_keys SET success_count = 0, fail_count = 0", [])?;
        Ok(n as u64)
    }

    /// 仅刷新 last_used_at（不计成功/失败）。
    /// 用于未走上游的请求（count_tokens / models / 本地错误），保证「最后使用时间」仍然准确。
    pub fn touch_api_key(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// 原子计数：调用成功 / 失败 + last_used_at
    pub fn record_api_key_outcome(&self, id: i64, ok: bool) -> Result<()> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();
        let sql = if ok {
            "UPDATE api_keys SET success_count = success_count + 1, last_used_at = ?2 WHERE id = ?1"
        } else {
            "UPDATE api_keys SET fail_count = fail_count + 1, last_used_at = ?2 WHERE id = ?1"
        };
        conn.execute(sql, params![id, now])?;
        Ok(())
    }

    // ============ Error logs ============

    /// 写入一条错误日志。返回新行 id。
    ///
    /// 同时维护每类累计计数（error_log_counters，修剪不影响），
    /// 并把该类日志修剪到最新 [`ERROR_LOG_MAX_PER_KIND`] 条。
    /// 三条语句包在同一事务：保证计数与日志原子一致，并合并为一次提交。
    pub fn insert_error_log(&self, log: &ErrorLogInsert) -> Result<i64> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO error_logs(at, credential_id, endpoint, status_code, upstream_status, \
             error_kind, model, summary, request_method, request_path, request_headers, \
             response_headers, request_body, response_body, user_id, request_id, disable_reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                log.at.to_rfc3339(),
                log.credential_id.map(|v| v as i64),
                log.endpoint.as_deref(),
                log.status_code as i64,
                log.upstream_status.map(|v| v as i64),
                log.error_kind.as_str(),
                log.model.as_deref(),
                log.summary.as_str(),
                log.request_method.as_deref(),
                log.request_path.as_deref(),
                log.request_headers.as_deref(),
                log.response_headers.as_deref(),
                log.request_body.as_deref(),
                log.response_body.as_deref(),
                log.user_id.as_deref(),
                log.request_id.as_deref(),
                log.disable_reason.as_deref(),
            ],
        )?;
        let id = tx.last_insert_rowid();

        // 累计计数（清空全部日志时才重置）
        tx.execute(
            "INSERT INTO error_log_counters(error_kind, total) VALUES (?1, 1) \
             ON CONFLICT(error_kind) DO UPDATE SET total = total + 1",
            params![log.error_kind.as_str()],
        )?;

        // 按类保留最新 N 条
        tx.execute(
            "DELETE FROM error_logs WHERE error_kind = ?1 AND id NOT IN (\
                SELECT id FROM error_logs WHERE error_kind = ?1 ORDER BY id DESC LIMIT ?2\
             )",
            params![log.error_kind.as_str(), ERROR_LOG_MAX_PER_KIND as i64],
        )?;

        tx.commit()?;
        Ok(id)
    }

    /// 各错误类型的累计次数（total，修剪/单条删除不影响）与当前留存条数（retained）。
    pub fn error_log_kind_stats(&self) -> Result<Vec<ErrorLogKindStat>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT c.error_kind, c.total, \
             (SELECT COUNT(*) FROM error_logs l WHERE l.error_kind = c.error_kind) \
             FROM error_log_counters c ORDER BY c.total DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ErrorLogKindStat {
                error_kind: r.get(0)?,
                total: r.get::<_, i64>(1)? as u64,
                retained: r.get::<_, i64>(2)? as u64,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// 列表查询：仅返回轻量摘要字段，request_body / response_body 不取，
    /// 让"日志页拉取"快得可控。返回 (条目, 命中总数)。
    pub fn list_error_logs(
        &self,
        filter: &ErrorLogFilter,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<ErrorLogSummary>, u64)> {
        let conn = self.conn()?;
        let mut where_clauses: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if !filter.status_codes.is_empty() {
            let placeholders: Vec<String> = filter
                .status_codes
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", args.len() + i + 1))
                .collect();
            where_clauses.push(format!("status_code IN ({})", placeholders.join(",")));
            for s in &filter.status_codes {
                args.push(Box::new(*s as i64));
            }
        }
        if !filter.error_kinds.is_empty() {
            let placeholders: Vec<String> = filter
                .error_kinds
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", args.len() + i + 1))
                .collect();
            where_clauses.push(format!("error_kind IN ({})", placeholders.join(",")));
            for k in &filter.error_kinds {
                args.push(Box::new(k.clone()));
            }
        }
        if let Some(cred) = filter.credential_id {
            where_clauses.push(format!("credential_id = ?{}", args.len() + 1));
            args.push(Box::new(cred as i64));
        }
        if let Some(since) = filter.since {
            where_clauses.push(format!("at >= ?{}", args.len() + 1));
            args.push(Box::new(since.to_rfc3339()));
        }
        if let Some(until) = filter.until {
            where_clauses.push(format!("at <= ?{}", args.len() + 1));
            args.push(Box::new(until.to_rfc3339()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_clauses.join(" AND "))
        };

        // 总数
        let count_sql = format!("SELECT COUNT(*) FROM error_logs{}", where_sql);
        let mut count_stmt = conn.prepare(&count_sql)?;
        let total: i64 = count_stmt.query_row(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |r| r.get(0),
        )?;

        // 列表
        let list_sql = format!(
            "SELECT id, at, credential_id, endpoint, status_code, upstream_status, error_kind, \
             model, summary, disable_reason FROM error_logs{} ORDER BY id DESC LIMIT ?{} OFFSET ?{}",
            where_sql,
            args.len() + 1,
            args.len() + 2,
        );
        let mut stmt = conn.prepare(&list_sql)?;
        let mut params_iter: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let limit_box: i64 = limit.min(500) as i64;
        let offset_box: i64 = offset as i64;
        params_iter.push(&limit_box);
        params_iter.push(&offset_box);
        let rows = stmt.query_map(rusqlite::params_from_iter(params_iter), |r| {
            let at_str: String = r.get(1)?;
            let cred_id: Option<i64> = r.get(2)?;
            let upstream_status: Option<i64> = r.get(5)?;
            Ok(ErrorLogSummary {
                id: r.get(0)?,
                at: parse_dt(&at_str).unwrap_or_else(Utc::now),
                credential_id: cred_id.map(|v| v as u64),
                endpoint: r.get(3)?,
                status_code: r.get::<_, i64>(4)? as u16,
                upstream_status: upstream_status.map(|v| v as u16),
                error_kind: r.get(6)?,
                model: r.get(7)?,
                summary: r.get(8)?,
                disable_reason: r.get(9)?,
            })
        })?;
        let items: Vec<ErrorLogSummary> = rows.collect::<rusqlite::Result<_>>()?;
        Ok((items, total as u64))
    }

    /// 详情：取完整字段，包括请求/响应体
    pub fn get_error_log(&self, id: i64) -> Result<Option<ErrorLogRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, at, credential_id, endpoint, status_code, upstream_status, error_kind, \
             model, summary, request_method, request_path, request_headers, response_headers, \
             request_body, response_body, user_id, request_id, disable_reason \
             FROM error_logs WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(r) = rows.next()? {
            let at_str: String = r.get(1)?;
            let cred_id: Option<i64> = r.get(2)?;
            let upstream_status: Option<i64> = r.get(5)?;
            Ok(Some(ErrorLogRow {
                id: r.get(0)?,
                at: parse_dt(&at_str).unwrap_or_else(Utc::now),
                credential_id: cred_id.map(|v| v as u64),
                endpoint: r.get(3)?,
                status_code: r.get::<_, i64>(4)? as u16,
                upstream_status: upstream_status.map(|v| v as u16),
                error_kind: r.get(6)?,
                model: r.get(7)?,
                summary: r.get(8)?,
                request_method: r.get(9)?,
                request_path: r.get(10)?,
                request_headers: r.get(11)?,
                response_headers: r.get(12)?,
                request_body: r.get(13)?,
                response_body: r.get(14)?,
                user_id: r.get(15)?,
                request_id: r.get(16)?,
                disable_reason: r.get(17)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn delete_error_log(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute("DELETE FROM error_logs WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn clear_error_logs(&self, before: Option<DateTime<Utc>>) -> Result<u64> {
        let conn = self.conn()?;
        let n = match before {
            Some(t) => conn.execute(
                "DELETE FROM error_logs WHERE at < ?1",
                params![t.to_rfc3339()],
            )?,
            None => {
                // 清空全部时同步重置各类累计计数
                let n = conn.execute("DELETE FROM error_logs", [])?;
                conn.execute("DELETE FROM error_log_counters", [])?;
                n
            }
        };
        Ok(n as u64)
    }

    /// 按数量上限 + 按天数清理旧日志。返回删除条数。
    /// max_count=0 表示不按数量限；max_age_days=0 表示不按天数限。
    ///
    /// 与 insert 时的按类保留（[`ERROR_LOG_MAX_PER_KIND`]）叠加生效：
    /// 按类保留把表大小封顶在「类型数 × 100」；本方法是全局兜底，
    /// 用户把 max_count / max_age_days 配得更小时可把留存进一步压低
    ///（此时前端的 retained 可能低于 100，属预期）。累计计数不受影响。
    pub fn prune_error_logs(&self, max_count: u64, max_age_days: u32) -> Result<u64> {
        let conn = self.conn()?;
        let mut deleted: u64 = 0;

        if max_age_days > 0 {
            let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);
            let n = conn.execute(
                "DELETE FROM error_logs WHERE at < ?1",
                params![cutoff.to_rfc3339()],
            )?;
            deleted += n as u64;
        }

        if max_count > 0 {
            // 保留最新的 max_count 条，删除剩余
            let n = conn.execute(
                "DELETE FROM error_logs WHERE id NOT IN (\
                    SELECT id FROM error_logs ORDER BY id DESC LIMIT ?1\
                 )",
                params![max_count as i64],
            )?;
            deleted += n as u64;
        }

        Ok(deleted)
    }
}

#[derive(Debug, Clone)]
pub struct BalanceCacheRow {
    pub remaining: f64,
    pub usage_limit: f64,
    pub usage_percentage: f64,
    pub subscription_title: Option<String>,
    pub cached_at: f64,
}

// ============ Error logs ============

/// 每种错误类型（error_kind）最多保留的最新日志条数
pub const ERROR_LOG_MAX_PER_KIND: u64 = 100;

/// 每类错误的累计次数与当前留存条数
#[derive(Debug, Clone)]
pub struct ErrorLogKindStat {
    pub error_kind: String,
    /// 累计发生次数（修剪/单条删除不影响；「清空全部」时归零）
    pub total: u64,
    /// 当前留存条数（≤ ERROR_LOG_MAX_PER_KIND）
    pub retained: u64,
}

/// 写入错误日志的输入结构
#[derive(Debug, Clone)]
pub struct ErrorLogInsert {
    pub at: DateTime<Utc>,
    pub credential_id: Option<u64>,
    pub endpoint: Option<String>,
    pub status_code: u16,
    pub upstream_status: Option<u16>,
    pub error_kind: String,
    pub model: Option<String>,
    pub summary: String,
    pub request_method: Option<String>,
    pub request_path: Option<String>,
    pub request_headers: Option<String>,
    pub response_headers: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub user_id: Option<String>,
    pub request_id: Option<String>,
    /// 凭据被禁用时的原因（AccountSuspended / AuthenticationFailed 等）；非禁用事件为 None
    pub disable_reason: Option<String>,
}

/// 列表查询过滤条件
#[derive(Debug, Default, Clone)]
pub struct ErrorLogFilter {
    pub status_codes: Vec<u16>,
    pub error_kinds: Vec<String>,
    pub credential_id: Option<u64>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// 列表项（不含大字段）
#[derive(Debug, Clone)]
pub struct ErrorLogSummary {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub credential_id: Option<u64>,
    pub endpoint: Option<String>,
    pub status_code: u16,
    pub upstream_status: Option<u16>,
    pub error_kind: String,
    pub model: Option<String>,
    pub summary: String,
    pub disable_reason: Option<String>,
}

/// 详情（含完整请求/响应体）
#[derive(Debug, Clone)]
pub struct ErrorLogRow {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub credential_id: Option<u64>,
    pub endpoint: Option<String>,
    pub status_code: u16,
    pub upstream_status: Option<u16>,
    pub error_kind: String,
    pub model: Option<String>,
    pub summary: String,
    pub request_method: Option<String>,
    pub request_path: Option<String>,
    pub request_headers: Option<String>,
    pub response_headers: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub user_id: Option<String>,
    pub request_id: Option<String>,
    pub disable_reason: Option<String>,
}

// ============ row mappers ============

fn row_to_credentials(row: &rusqlite::Row<'_>) -> rusqlite::Result<KiroCredentials> {
    let id: i64 = row.get(0)?;
    let priority: i64 = row.get(9)?;
    let disabled: i64 = row.get(17)?;
    let allow_overuse: i64 = row.get(18)?;
    let rpm_raw: Option<i64> = row.get(19).ok();
    let last_overage_status: Option<String> = row.get(20).ok().flatten();
    Ok(KiroCredentials {
        runtime_only: false,
        id: Some(id as u64),
        access_token: row.get(1)?,
        refresh_token: row.get(2)?,
        kiro_api_key: row.get(3)?,
        profile_arn: row.get(4)?,
        expires_at: row.get(5)?,
        auth_method: row.get(6)?,
        client_id: row.get(7)?,
        client_secret: row.get(8)?,
        priority: priority as u32,
        region: row.get(10)?,
        api_region: row.get(11)?,
        machine_id: row.get(12)?,
        endpoint: row.get(13)?,
        email: row.get(14)?,
        subscription_title: row.get(15)?,
        proxy_slot_id: row.get(16)?,
        disabled: disabled != 0,
        allow_overuse: allow_overuse != 0,
        rpm: rpm_raw.and_then(|v| {
            if v <= 0 {
                None
            } else if v > u32::MAX as i64 {
                Some(u32::MAX)
            } else {
                Some(v as u32)
            }
        }),
        last_overage_status,
    })
}

fn row_to_proxy_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxyEntry> {
    let expires_str: String = row.get(4)?;
    let created_str: String = row.get(7)?;
    let last_rotated_str: Option<String> = row.get(8)?;
    Ok(ProxyEntry {
        id: row.get(0)?,
        url: row.get(1)?,
        username: row.get(2)?,
        password: row.get(3)?,
        expires_at: parse_dt(&expires_str).unwrap_or_else(Utc::now),
        slots: row.get::<_, i64>(5)? as u32,
        bound_credential_ids: vec![],
        label: row.get(6)?,
        created_at: parse_dt(&created_str).unwrap_or_else(Utc::now),
        last_rotated_at: last_rotated_str.as_deref().and_then(parse_dt),
        disabled: row.get::<_, i64>(9)? != 0,
        disabled_category: row
            .get::<_, Option<String>>(10)?
            .as_deref()
            .and_then(ProxyDisabledCategory::from_str_opt),
        disabled_reason: row.get(11)?,
    })
}

fn parse_dt(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn insert_credential(tx: &rusqlite::Transaction<'_>, c: &KiroCredentials) -> Result<()> {
    let id = c.id.ok_or_else(|| anyhow::anyhow!("credential 无 id"))?;
    tx.execute(
        "INSERT INTO credentials(id, access_token, refresh_token, kiro_api_key, profile_arn, expires_at, \
         auth_method, client_id, client_secret, priority, region, api_region, machine_id, endpoint, \
         email, subscription_title, proxy_slot_id, disabled, allow_overuse, rpm, last_overage_status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![
            id as i64,
            c.access_token,
            c.refresh_token,
            c.kiro_api_key,
            c.profile_arn,
            c.expires_at,
            c.auth_method,
            c.client_id,
            c.client_secret,
            c.priority as i64,
            c.region,
            c.api_region,
            c.machine_id,
            c.endpoint,
            c.email,
            c.subscription_title,
            c.proxy_slot_id,
            if c.disabled { 1 } else { 0 } as i64,
            if c.allow_overuse { 1 } else { 0 } as i64,
            c.rpm.map(|v| v as i64),
            c.last_overage_status.as_deref(),
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
fn upsert_credential_inner(conn: &Conn, c: &KiroCredentials) -> Result<()> {
    let id = c.id.ok_or_else(|| anyhow::anyhow!("credential 无 id"))?;
    conn.execute(
        "INSERT INTO credentials(id, access_token, refresh_token, kiro_api_key, profile_arn, expires_at, \
         auth_method, client_id, client_secret, priority, region, api_region, machine_id, endpoint, \
         email, subscription_title, proxy_slot_id, disabled, allow_overuse, rpm, last_overage_status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21) \
         ON CONFLICT(id) DO UPDATE SET \
            access_token=excluded.access_token, refresh_token=excluded.refresh_token, \
            kiro_api_key=excluded.kiro_api_key, profile_arn=excluded.profile_arn, \
            expires_at=excluded.expires_at, auth_method=excluded.auth_method, \
            client_id=excluded.client_id, client_secret=excluded.client_secret, \
            priority=excluded.priority, region=excluded.region, api_region=excluded.api_region, \
            machine_id=excluded.machine_id, endpoint=excluded.endpoint, email=excluded.email, \
            subscription_title=excluded.subscription_title, proxy_slot_id=excluded.proxy_slot_id, \
            disabled=excluded.disabled, allow_overuse=excluded.allow_overuse, rpm=excluded.rpm, \
            last_overage_status=excluded.last_overage_status",
        params![
            id as i64,
            c.access_token,
            c.refresh_token,
            c.kiro_api_key,
            c.profile_arn,
            c.expires_at,
            c.auth_method,
            c.client_id,
            c.client_secret,
            c.priority as i64,
            c.region,
            c.api_region,
            c.machine_id,
            c.endpoint,
            c.email,
            c.subscription_title,
            c.proxy_slot_id,
            if c.disabled { 1 } else { 0 } as i64,
            if c.allow_overuse { 1 } else { 0 } as i64,
            c.rpm.map(|v| v as i64),
            c.last_overage_status.as_deref(),
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub struct StoreHandle(pub Arc<Store>);

#[allow(dead_code)]
pub type SharedStore = RwLock<Option<Arc<Store>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_store() -> (Arc<Store>, PathBuf) {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kiro_rpm_test_{}_{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).expect("open store");
        (store, path)
    }

    #[test]
    fn test_record_rpm_with_rl_count_and_analysis() {
        let (store, path) = temp_store();
        let now_min = Utc::now().timestamp() / 60;

        // 号 1：低 RPM 无 429；高 RPM 有 429
        store.record_rpm(1, now_min - 2, 10, 0).unwrap();
        store.record_rpm(1, now_min - 1, 50, 0).unwrap();
        store.record_rpm(1, now_min, 90, 9).unwrap();
        // 号 2：单点
        store.record_rpm(2, now_min, 30, 1).unwrap();

        // 聚合历史仍只看 count，不受 rl_count 影响
        let agg = store.rpm_history_aggregate(1).unwrap();
        let total: u32 = agg.iter().map(|(_, c)| *c).sum();
        assert_eq!(total, 10 + 50 + 90 + 30);

        // 分析数据带回 rl_count
        let mut rows = store.rpm_analysis_all(1).unwrap();
        rows.sort();
        assert!(rows.contains(&(1, 90, 9)));
        assert!(rows.contains(&(2, 30, 1)));
        assert_eq!(rows.iter().filter(|(id, _, _)| *id == 1).count(), 3);

        // ON CONFLICT 覆盖：同分钟重写应更新 count 和 rl_count
        store.record_rpm(1, now_min, 95, 12).unwrap();
        let rows2 = store.rpm_analysis_all(1).unwrap();
        assert!(rows2.contains(&(1, 95, 12)));
        assert!(!rows2.contains(&(1, 90, 9)));

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    fn error_log(kind: &str, summary: &str) -> ErrorLogInsert {
        ErrorLogInsert {
            at: Utc::now(),
            credential_id: Some(1),
            endpoint: None,
            status_code: 502,
            upstream_status: Some(500),
            error_kind: kind.to_string(),
            model: None,
            summary: summary.to_string(),
            request_method: None,
            request_path: None,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
            user_id: None,
            request_id: None,
            disable_reason: None,
        }
    }

    #[test]
    fn test_error_log_per_kind_retention_and_counters() {
        let (store, path) = temp_store();

        // 写入超过上限的同类日志：只留存最新 ERROR_LOG_MAX_PER_KIND 条，累计计数不受修剪影响
        let overflow = ERROR_LOG_MAX_PER_KIND + 20;
        for i in 0..overflow {
            store
                .insert_error_log(&error_log("rate_limit", &format!("rl-{}", i)))
                .unwrap();
        }
        // 另一类少量写入，验证互不影响
        for i in 0..3 {
            store
                .insert_error_log(&error_log("auth", &format!("auth-{}", i)))
                .unwrap();
        }

        let stats = store.error_log_kind_stats().unwrap();
        let rl = stats.iter().find(|s| s.error_kind == "rate_limit").unwrap();
        assert_eq!(rl.total, overflow);
        assert_eq!(rl.retained, ERROR_LOG_MAX_PER_KIND);
        let auth = stats.iter().find(|s| s.error_kind == "auth").unwrap();
        assert_eq!(auth.total, 3);
        assert_eq!(auth.retained, 3);

        // 留存的是最新的：最旧的 20 条已被修剪
        let (items, total) = store
            .list_error_logs(
                &ErrorLogFilter {
                    error_kinds: vec!["rate_limit".to_string()],
                    ..Default::default()
                },
                500,
                0,
            )
            .unwrap();
        assert_eq!(total, ERROR_LOG_MAX_PER_KIND);
        assert!(items.iter().all(|it| {
            let n: u64 = it.summary.trim_start_matches("rl-").parse().unwrap();
            n >= 20
        }));

        // 单条删除：留存 -1，累计不变
        let first_id = items[0].id;
        assert!(store.delete_error_log(first_id).unwrap());
        let stats = store.error_log_kind_stats().unwrap();
        let rl = stats.iter().find(|s| s.error_kind == "rate_limit").unwrap();
        assert_eq!(rl.total, overflow);
        assert_eq!(rl.retained, ERROR_LOG_MAX_PER_KIND - 1);

        // 清空全部：累计计数一并归零
        store.clear_error_logs(None).unwrap();
        assert!(store.error_log_kind_stats().unwrap().is_empty());

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_error_log_counters_backfill_on_upgrade() {
        let (store, path) = temp_store();
        // 模拟旧版本库：只有日志行、无累计计数
        store.insert_error_log(&error_log("auth", "a")).unwrap();
        store
            .insert_error_log(&error_log("rate_limit", "b"))
            .unwrap();
        store
            .insert_error_log(&error_log("rate_limit", "c"))
            .unwrap();
        {
            let conn = store.conn().unwrap();
            conn.execute("DELETE FROM error_log_counters", []).unwrap();
        }
        drop(store);

        // 升级路径：重新打开时 ensure_schema 用存量日志回填计数
        let store = Store::open(&path).expect("reopen store");
        let stats = store.error_log_kind_stats().unwrap();
        let rl = stats.iter().find(|s| s.error_kind == "rate_limit").unwrap();
        assert_eq!((rl.total, rl.retained), (2, 2));
        let auth = stats.iter().find(|s| s.error_kind == "auth").unwrap();
        assert_eq!((auth.total, auth.retained), (1, 1));

        // 计数已存在时再次打开不得重复回填
        drop(store);
        let store = Store::open(&path).expect("reopen store again");
        let stats = store.error_log_kind_stats().unwrap();
        let rl = stats.iter().find(|s| s.error_kind == "rate_limit").unwrap();
        assert_eq!(rl.total, 2);

        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
