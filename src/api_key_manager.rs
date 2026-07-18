//! API Key 管理器（运行时索引 + 并发计数 + 缓存比例配置）
//!
//! - 内存索引 `HashMap<key_string, Arc<ApiKeyEntry>>`
//! - 每条 entry 含 `in_flight: AtomicU32`，用于并发限制
//! - `authorize(key)` → 通过则返回 RAII guard（drop 时 dec）；失败返回错误
//! - 写盘：`record_outcome` 异步调用 sqlite 更新 `success_count/fail_count/last_used_at`
//! - 缓存比例：`cache_read_min_pct..=cache_read_max_pct`（0..=100），运行时按 key 取随机值

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::RwLock;

use crate::storage::{ApiKeyRow, Store};

/// 单条 API Key 的运行时状态
#[allow(dead_code)]
pub struct ApiKeyEntry {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub enabled: bool,
    pub max_concurrent: u32,
    pub cache_read_min_pct: u32,
    pub cache_read_max_pct: u32,
    /// 并发计数器。用 Arc 包裹使其跨 reload 存活：reload 重建 entry 时复用
    /// 同一个计数器，在途请求的 guard（持有旧 entry）drop 时减的仍是它，
    /// 避免「复制数值 + 旧 guard 减旧计数器」导致的永久虚高。
    pub in_flight: Arc<AtomicU32>,
    /// 允许使用的凭据 ID 集合（None = 全部可用；Some 一定非空）
    pub allowed_credentials: Option<Arc<HashSet<u64>>>,
}

impl ApiKeyEntry {
    pub fn from_row(row: &ApiKeyRow) -> Arc<Self> {
        Self::from_row_with_counter(row, Arc::new(AtomicU32::new(0)))
    }

    /// 用指定的并发计数器构建 entry（reload 时传入旧 entry 的计数器以保持连续）
    pub fn from_row_with_counter(row: &ApiKeyRow, in_flight: Arc<AtomicU32>) -> Arc<Self> {
        // 空列表 = 全部可用 → None；否则收敛为非空 HashSet
        let allowed_credentials = if row.allowed_credentials.is_empty() {
            None
        } else {
            Some(Arc::new(
                row.allowed_credentials
                    .iter()
                    .copied()
                    .collect::<HashSet<u64>>(),
            ))
        };
        Arc::new(Self {
            id: row.id,
            key: row.key.clone(),
            name: row.name.clone(),
            enabled: row.enabled,
            max_concurrent: row.max_concurrent,
            cache_read_min_pct: row.cache_read_min_pct,
            cache_read_max_pct: row.cache_read_max_pct,
            in_flight,
            allowed_credentials,
        })
    }

    /// 允许使用的凭据集合（None = 不限制，全部可用）
    pub fn allowed_credentials(&self) -> Option<Arc<HashSet<u64>>> {
        self.allowed_credentials.clone()
    }

    /// 抽样一个 cache_read 比例（百分比）。两端 0 视为不模拟，返回 None。
    pub fn sample_cache_read_pct(&self) -> Option<u32> {
        if self.cache_read_min_pct == 0 && self.cache_read_max_pct == 0 {
            return None;
        }
        let lo = self.cache_read_min_pct.min(100);
        let hi = self.cache_read_max_pct.min(100).max(lo);
        if lo == hi {
            Some(lo)
        } else {
            Some(fastrand::u32(lo..=hi))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("API Key 无效")]
    NotFound,
    #[error("API Key 已禁用")]
    Disabled,
    #[error("已达并发上限 {limit}（当前 {current}）")]
    ConcurrencyLimit { limit: u32, current: u32 },
}

/// 认证通过后的 RAII Guard
///
/// drop 时根据 `outcome` 字段触发 success/fail 计数（异步写盘），
/// 同时把 in_flight 减 1。
pub struct AuthGuard {
    pub entry: Arc<ApiKeyEntry>,
    /// 调用方在请求处理结束前调 `mark_success()` / `mark_fail()`；
    /// 默认值：未设置 → drop 时不计数（本地错误 / 未走上游的请求）
    outcome: Option<bool>,
    store: Option<Arc<Store>>,
}

#[allow(dead_code)]
impl AuthGuard {
    pub fn key_id(&self) -> i64 {
        self.entry.id
    }

    pub fn entry(&self) -> Arc<ApiKeyEntry> {
        self.entry.clone()
    }

    pub fn mark_success(&mut self) {
        self.outcome = Some(true);
    }

    pub fn mark_fail(&mut self) {
        self.outcome = Some(false);
    }
}

impl Drop for AuthGuard {
    fn drop(&mut self) {
        let prev = self.entry.in_flight.fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            self.entry.in_flight.fetch_add(1, Ordering::Relaxed);
        }
        // 计数：outcome=None（未走上游 / 本地错误）不计成功失败，
        // 但仍刷新 last_used_at——count_tokens / models 也算「在使用」
        let outcome = self.outcome;
        let store = self.store.clone();
        let id = self.entry.id;
        if let Some(store) = store {
            tokio::spawn(async move {
                if let Err(e) = tokio::task::spawn_blocking(move || match outcome {
                    Some(ok) => store.record_api_key_outcome(id, ok),
                    None => store.touch_api_key(id),
                })
                .await
                .unwrap_or_else(|_| Ok(()))
                {
                    tracing::warn!(api_key_id = id, "记录 API Key 计数失败: {}", e);
                }
            });
        }
    }
}

/// 全局 API Key 管理器
pub struct ApiKeyManager {
    keys: RwLock<HashMap<String, Arc<ApiKeyEntry>>>,
    store: Arc<Store>,
}

impl ApiKeyManager {
    /// 从 SQLite 加载所有 keys
    pub fn load(store: Arc<Store>) -> anyhow::Result<Arc<Self>> {
        let rows = store.list_api_keys()?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in &rows {
            map.insert(row.key.clone(), ApiKeyEntry::from_row(row));
        }
        Ok(Arc::new(Self {
            keys: RwLock::new(map),
            store,
        }))
    }

    /// 重新从 DB 加载（管理员 CRUD 后调用）
    pub fn reload(&self) -> anyhow::Result<()> {
        let rows = self.store.list_api_keys()?;
        let mut new_map = HashMap::with_capacity(rows.len());
        for row in &rows {
            // 复用旧 entry 的同一个 in_flight 计数器（不是复制数值）：
            // 在途请求的 guard 持有旧 entry，drop 时减的就是这同一个 Atomic，
            // 否则新 entry 从 N 起步且永远没人减，会永久虚高并占掉并发名额
            let entry = if let Some(old) = self.keys.read().get(&row.key) {
                ApiKeyEntry::from_row_with_counter(row, old.in_flight.clone())
            } else {
                ApiKeyEntry::from_row(row)
            };
            new_map.insert(row.key.clone(), entry);
        }
        *self.keys.write() = new_map;
        Ok(())
    }

    /// 鉴权 + 占并发位
    pub fn authorize(&self, key: &str) -> Result<AuthGuard, AuthError> {
        let entry = self
            .keys
            .read()
            .get(key)
            .cloned()
            .ok_or(AuthError::NotFound)?;
        if !entry.enabled {
            return Err(AuthError::Disabled);
        }
        if entry.max_concurrent > 0 {
            // CAS 风格抢占：若 inc 后超限则回滚
            let prev = entry.in_flight.fetch_add(1, Ordering::Relaxed);
            if prev >= entry.max_concurrent {
                entry.in_flight.fetch_sub(1, Ordering::Relaxed);
                return Err(AuthError::ConcurrencyLimit {
                    limit: entry.max_concurrent,
                    current: prev,
                });
            }
        } else {
            entry.in_flight.fetch_add(1, Ordering::Relaxed);
        }
        Ok(AuthGuard {
            entry,
            outcome: None,
            store: Some(self.store.clone()),
        })
    }

    /// 取 key 对应的 entry（不占并发位，用于纯查询）
    pub fn get(&self, key: &str) -> Option<Arc<ApiKeyEntry>> {
        self.keys.read().get(key).cloned()
    }

    /// 当前所有 keys 的 in_flight 快照
    pub fn snapshot_in_flight(&self) -> HashMap<i64, u32> {
        self.keys
            .read()
            .values()
            .map(|e| (e.id, e.in_flight.load(Ordering::Relaxed)))
            .collect()
    }
}

/// 把 cache_read_pct 应用到一组 usage 字段：
/// 输入 (input, cache_read, cache_creation)，输出 (input', cache_read', cache_creation')。
/// 总和（input + cache_read + cache_creation）始终守恒。
///
/// `scale_hit_only` 选择两种模式：
/// - `true`（缩放真实命中，默认）：只把真实命中的 `cache_read` 按 pct% 缩放，
///   未命中（read=0）则 cache_read 保持 0、不伪造；真实 `cache_creation` 原样保留；
///   被缩掉的命中部分回落到 input。
/// - `false`（按总输入比例，旧行为）：不论是否命中，新 cache_read = total * pct%，
///   其余分到 input，cache_creation 清零（避免重复计费）。
pub fn apply_cache_simulation(
    input_tokens: i32,
    cache_read_input_tokens: i32,
    cache_creation_input_tokens: i32,
    pct: u32,
    scale_hit_only: bool,
) -> (i32, i32, i32) {
    let total = input_tokens
        .saturating_add(cache_read_input_tokens)
        .saturating_add(cache_creation_input_tokens);
    if total <= 0 {
        return (
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
    }
    let pct = pct.min(100) as i32;

    if scale_hit_only {
        let new_read = (cache_read_input_tokens.max(0) * pct) / 100;
        let new_creation = cache_creation_input_tokens.max(0);
        let new_input = (total - new_read - new_creation).max(0);
        (new_input, new_read, new_creation)
    } else {
        let new_read = (total * pct) / 100;
        let new_input = total - new_read;
        (new_input, new_read, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Arc<Store> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kiro_apikey_test_{}_{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        Store::open(&path).expect("open store")
    }

    /// reload 必须复用同一个 in_flight 计数器：
    /// 在途请求（guard 持有旧 entry）在 reload 之后 drop，计数应回到 0，
    /// 而不是把 reload 时复制的数值永久留在新 entry 上。
    #[tokio::test]
    async fn test_reload_shares_in_flight_counter() {
        let store = temp_store();
        store
            .create_api_key(&crate::storage::ApiKeyCreate {
                key: "sk-test-reload".into(),
                name: "t".into(),
                description: None,
                enabled: true,
                max_concurrent: 0,
                cache_read_min_pct: 0,
                cache_read_max_pct: 0,
                allowed_credentials: vec![],
            })
            .expect("create key");
        let mgr = ApiKeyManager::load(store).expect("load");

        let guard = mgr.authorize("sk-test-reload").expect("authorize");
        let id = guard.key_id();
        assert_eq!(mgr.snapshot_in_flight().get(&id), Some(&1));

        // 模拟 admin CRUD 触发的 reload（此时请求仍在途）
        mgr.reload().expect("reload");
        assert_eq!(
            mgr.snapshot_in_flight().get(&id),
            Some(&1),
            "reload 后在途计数应保留"
        );

        // 在途请求结束：guard 减的应是 reload 后的同一个计数器
        drop(guard);
        assert_eq!(
            mgr.snapshot_in_flight().get(&id),
            Some(&0),
            "guard drop 后计数应归零，而不是永久虚高"
        );
    }

    // ---- 旧模式：按总输入比例（scale_hit_only = false）----
    #[test]
    fn test_apply_cache_simulation_50pct() {
        let (i, r, c) = apply_cache_simulation(800, 100, 100, 50, false);
        assert_eq!(i + r + c, 1000);
        assert_eq!(r, 500);
        assert_eq!(c, 0);
    }

    #[test]
    fn test_apply_cache_simulation_zero_total() {
        let (i, r, c) = apply_cache_simulation(0, 0, 0, 50, false);
        assert_eq!((i, r, c), (0, 0, 0));
    }

    #[test]
    fn test_apply_cache_simulation_100pct() {
        let (i, r, c) = apply_cache_simulation(1000, 0, 0, 100, false);
        assert_eq!(r, 1000);
        assert_eq!(i, 0);
        assert_eq!(c, 0);
    }

    // ---- 新模式：缩放真实命中（scale_hit_only = true）----
    #[test]
    fn test_scale_hit_scales_real_read() {
        // 真实命中 800，creation 50，input 150（total=1000）；pct=50
        // → read = 800*50% = 400；creation 保留 50；input = 1000-400-50 = 550
        let (i, r, c) = apply_cache_simulation(150, 800, 50, 50, true);
        assert_eq!(i + r + c, 1000);
        assert_eq!(r, 400);
        assert_eq!(c, 50);
        assert_eq!(i, 550);
    }

    #[test]
    fn test_scale_hit_no_hit_does_not_fabricate() {
        // 真实未命中（read=0），无论 pct 多少都不应伪造 cache_read
        let (i, r, c) = apply_cache_simulation(900, 0, 100, 80, true);
        assert_eq!(r, 0);
        assert_eq!(c, 100);
        assert_eq!(i, 900);
        assert_eq!(i + r + c, 1000);
    }

    #[test]
    fn test_scale_hit_100pct_keeps_full_hit() {
        // pct=100 时真实命中全保留
        let (i, r, c) = apply_cache_simulation(200, 800, 0, 100, true);
        assert_eq!(r, 800);
        assert_eq!(c, 0);
        assert_eq!(i, 200);
    }
}
