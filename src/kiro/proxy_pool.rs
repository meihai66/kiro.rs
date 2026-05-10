//! 代理池模块
//!
//! 管理一组带到期时间和槽位容量的代理。凭据通过 `proxy_slot_id`
//! 指向池里的某个代理，所有出站（API 请求、Token 刷新、余额查询）
//! 都按凭据所绑代理走代理，**不允许回退本地直连**。
//!
//! - 文件持久化：JSON 数组，原子写入（tmp + rename）
//! - 自动绑定：导入凭据时从池里挑"剩余槽位多 / 到期最远"的代理
//! - 后台轮换：剩余有效期 < 24h 时换到时间更充足的代理（见 proxy_rotation.rs）
//! - 1 个代理可有多个槽位（slots 字段），同一代理可绑定多个凭据

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::http_client::ProxyConfig;

/// 代理条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEntry {
    /// 唯一 ID（自动生成，形如 "p-xxxxxxxx"）
    pub id: String,

    /// 代理 URL，如 `http://host:port` / `socks5://host:port`
    pub url: String,

    /// 代理认证用户名（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// 到期时间（RFC3339）
    pub expires_at: DateTime<Utc>,

    /// 槽位容量：最多绑定多少个凭据
    #[serde(default = "default_slots")]
    pub slots: u32,

    /// 已绑定的凭据 ID 列表
    #[serde(default)]
    pub bound_credential_ids: Vec<u64>,

    /// 可选标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// 创建时间
    pub created_at: DateTime<Utc>,

    /// 上次被分配出去的时间（auto_bind / manual_bind / 轮换时更新）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotated_at: Option<DateTime<Utc>>,
}

fn default_slots() -> u32 {
    1
}

#[allow(dead_code)]
impl ProxyEntry {
    /// 已用槽位数量
    pub fn used_slots(&self) -> u32 {
        self.bound_credential_ids.len() as u32
    }

    /// 剩余可用槽位
    pub fn available_slots(&self) -> u32 {
        self.slots.saturating_sub(self.used_slots())
    }

    /// 是否可再容纳一个绑定
    pub fn has_free_slot(&self) -> bool {
        self.available_slots() > 0
    }

    /// 是否已过期
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    /// 剩余有效时间（秒，已过期返回 0）
    pub fn remaining_secs(&self, now: DateTime<Utc>) -> i64 {
        (self.expires_at - now).num_seconds().max(0)
    }

    /// 转为 ProxyConfig（运行时使用）
    pub fn to_proxy_config(&self) -> ProxyConfig {
        let mut proxy = ProxyConfig::new(&self.url);
        if let (Some(u), Some(p)) = (&self.username, &self.password) {
            proxy = proxy.with_auth(u, p);
        }
        proxy
    }
}

/// 池告警事件
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyAlert {
    pub at: DateTime<Utc>,
    pub level: AlertLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Warn,
    Error,
    Info,
}

const ALERT_BUFFER_CAP: usize = 100;

/// 代理"最近失败"冷却时长：被标记后 5 分钟内不会被自动 prepick / auto_bind 选中。
/// 防止批量导入时少数坏代理被反复挑中、把后续凭据全部拖下水。
const RECENT_FAILURE_COOLDOWN: Duration = Duration::from_secs(300);

/// 代理池（共享、线程安全）
pub struct ProxyPool {
    entries: RwLock<Vec<ProxyEntry>>,
    file_path: RwLock<Option<PathBuf>>,
    alerts: RwLock<std::collections::VecDeque<ProxyAlert>>,
    /// SQLite 持久化（启动期注入；启用后写入走 SQL 而非 JSON）
    store: RwLock<Option<Arc<crate::storage::Store>>>,
    /// 内存里的"最近失败"标记（slot_id → 失败时刻），进程重启自动清空。
    /// 仅作用于自动选择路径（pick_idle_candidate / auto_bind），不影响 manual_bind。
    recent_failures: RwLock<HashMap<String, Instant>>,
}

#[allow(dead_code)]
impl ProxyPool {
    /// 创建空池（不绑定文件）
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(Vec::new()),
            file_path: RwLock::new(None),
            alerts: RwLock::new(std::collections::VecDeque::with_capacity(ALERT_BUFFER_CAP)),
            store: RwLock::new(None),
            recent_failures: RwLock::new(HashMap::new()),
        })
    }

    /// 启用代理池后，从 SQLite 加载初始数据
    pub fn from_store(store: Arc<crate::storage::Store>) -> anyhow::Result<Arc<Self>> {
        let entries = store.list_proxies()?;
        Ok(Arc::new(Self {
            entries: RwLock::new(entries),
            file_path: RwLock::new(None),
            alerts: RwLock::new(std::collections::VecDeque::with_capacity(ALERT_BUFFER_CAP)),
            store: RwLock::new(Some(store)),
            recent_failures: RwLock::new(HashMap::new()),
        }))
    }

    /// 从文件加载（不存在则创建空池但绑定路径，后续保存就能写入）
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Arc<Self>> {
        let path = path.as_ref();
        let entries = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("读取代理池文件失败: {}", path.display()))?;
            if content.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str::<Vec<ProxyEntry>>(&content)
                    .with_context(|| format!("解析代理池文件失败: {}", path.display()))?
            }
        } else {
            Vec::new()
        };

        Ok(Arc::new(Self {
            entries: RwLock::new(entries),
            file_path: RwLock::new(Some(path.to_path_buf())),
            alerts: RwLock::new(std::collections::VecDeque::with_capacity(ALERT_BUFFER_CAP)),
            store: RwLock::new(None),
            recent_failures: RwLock::new(HashMap::new()),
        }))
    }

    /// 标记代理"最近失败"——5 分钟内不会被自动 prepick / auto_bind 选中。
    /// 用于网络层失败（reqwest 连不上代理或经代理访问目标失败），避免坏代理被反复挑中。
    pub fn mark_recent_failure(&self, slot_id: &str) {
        self.recent_failures
            .write()
            .insert(slot_id.to_string(), Instant::now());
    }

    /// 清除"最近失败"标记（验证成功后调用，可选）
    pub fn clear_recent_failure(&self, slot_id: &str) {
        self.recent_failures.write().remove(slot_id);
    }

    /// 是否在失败冷却内
    fn is_in_failure_cooldown(&self, slot_id: &str) -> bool {
        self.recent_failures
            .read()
            .get(slot_id)
            .is_some_and(|t| t.elapsed() < RECENT_FAILURE_COOLDOWN)
    }

    /// 仅查询：返回最优空闲代理 ID，过滤已试过的 + 冷却中的
    pub fn pick_idle_candidate_excluding(
        &self,
        min_validity_hours: i64,
        exclude: &std::collections::HashSet<String>,
    ) -> Option<String> {
        let now = Utc::now();
        let entries = self.entries.read();
        entries
            .iter()
            .filter(|e| {
                e.has_free_slot()
                    && (e.expires_at - now).num_hours() > min_validity_hours
                    && !exclude.contains(&e.id)
                    && !self.is_in_failure_cooldown(&e.id)
            })
            .max_by(|a, b| {
                let by_avail = a.available_slots().cmp(&b.available_slots());
                if by_avail == std::cmp::Ordering::Equal {
                    a.expires_at.cmp(&b.expires_at)
                } else {
                    by_avail
                }
            })
            .map(|e| e.id.clone())
    }

    /// 启动期注入 SQLite store（之后写入走 SQL 而非 JSON）
    pub fn set_store(&self, store: Arc<crate::storage::Store>) {
        *self.store.write() = Some(store);
    }

    /// 当前文件路径
    pub fn file_path(&self) -> Option<PathBuf> {
        self.file_path.read().clone()
    }

    /// 持久化（仅 SQLite；测试时未绑 store 则静默 no-op）
    pub fn save(&self) -> anyhow::Result<()> {
        let store = match self.store.read().clone() {
            Some(s) => s,
            None => return Ok(()), // 未绑 store（如单元测试 ProxyPool::empty()）
        };
        let snapshot: Vec<ProxyEntry> = self.entries.read().clone();
        store
            .replace_all_proxies(&snapshot)
            .context("写入 SQLite 代理池失败")?;
        Ok(())
    }

    /// 当前所有条目快照
    pub fn snapshot(&self) -> Vec<ProxyEntry> {
        self.entries.read().clone()
    }

    /// 数量
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// 池是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// 按 id 取一份
    pub fn get(&self, id: &str) -> Option<ProxyEntry> {
        self.entries.read().iter().find(|e| e.id == id).cloned()
    }

    /// 取代理对应的 ProxyConfig（运行时调用，热路径）
    ///
    /// 返回 `Err` 表示：
    /// - 代理不存在
    /// - 代理已过期
    pub fn proxy_config_for(&self, slot_id: &str) -> anyhow::Result<ProxyConfig> {
        let now = Utc::now();
        let entries = self.entries.read();
        let entry = entries
            .iter()
            .find(|e| e.id == slot_id)
            .ok_or_else(|| anyhow::anyhow!("代理槽 {} 不存在", slot_id))?;
        if entry.is_expired(now) {
            anyhow::bail!("代理槽 {} 已过期", slot_id);
        }
        Ok(entry.to_proxy_config())
    }

    /// 仅查询：返回最优空闲代理 ID，不修改池状态
    ///
    /// 用于 add_credential 在"上游验证（refresh / getUsageLimits）"前占位选槽，
    /// 让验证阶段能用此槽走代理；真正绑定在验证成功后通过 `manual_bind` 完成。
    ///
    /// 选择策略：剩余槽位多优先 → 剩余有效期长优先；
    /// 仅返回剩余有效期 > `min_validity_hours` 的代理。
    pub fn pick_idle_candidate(&self, min_validity_hours: i64) -> Option<String> {
        let now = Utc::now();
        let entries = self.entries.read();
        entries
            .iter()
            .filter(|e| {
                e.has_free_slot()
                    && (e.expires_at - now).num_hours() > min_validity_hours
                    && !self.is_in_failure_cooldown(&e.id)
            })
            .max_by(|a, b| {
                let by_avail = a.available_slots().cmp(&b.available_slots());
                if by_avail == std::cmp::Ordering::Equal {
                    a.expires_at.cmp(&b.expires_at)
                } else {
                    by_avail
                }
            })
            .map(|e| e.id.clone())
    }

    /// 自动从池里选一个最优空闲代理
    ///
    /// 选择策略：剩余槽位多优先 → 剩余有效期长优先
    /// 仅返回剩余有效期 > `min_validity_hours` 的代理
    ///
    /// 成功返回代理 id，并把 credential_id 写入 bound_credential_ids（持久化）
    pub fn auto_bind(
        &self,
        credential_id: u64,
        min_validity_hours: i64,
    ) -> anyhow::Result<String> {
        let chosen_id = {
            let now = Utc::now();
            let mut entries = self.entries.write();

            // 在写锁内选择候选并直接绑定（跳过最近失败冷却中的代理）
            let chosen_idx = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.has_free_slot()
                        && (e.expires_at - now).num_hours() > min_validity_hours
                        && !e.bound_credential_ids.contains(&credential_id)
                        && !self.is_in_failure_cooldown(&e.id)
                })
                .max_by(|(_, a), (_, b)| {
                    let by_avail = a.available_slots().cmp(&b.available_slots());
                    if by_avail == std::cmp::Ordering::Equal {
                        a.expires_at.cmp(&b.expires_at)
                    } else {
                        by_avail
                    }
                })
                .map(|(idx, _)| idx);

            let idx = chosen_idx
                .ok_or_else(|| anyhow::anyhow!("代理池无可用代理（空闲槽 + 有效期充足）"))?;

            entries[idx].bound_credential_ids.push(credential_id);
            entries[idx].last_rotated_at = Some(now);
            entries[idx].id.clone()
        };

        self.save()?;
        Ok(chosen_id)
    }

    /// 手动指定绑定
    ///
    /// 校验：
    /// - 代理存在
    /// - 该代理仍有空槽（除非 credential 已经绑了它，幂等）
    pub fn manual_bind(&self, proxy_id: &str, credential_id: u64) -> anyhow::Result<()> {
        {
            let now = Utc::now();
            let mut entries = self.entries.write();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == proxy_id)
                .ok_or_else(|| anyhow::anyhow!("代理 {} 不存在", proxy_id))?;

            if entry.bound_credential_ids.contains(&credential_id) {
                return Ok(()); // 幂等
            }
            if !entry.has_free_slot() {
                anyhow::bail!(
                    "代理 {} 已无空闲槽位（{}/{}）",
                    proxy_id,
                    entry.used_slots(),
                    entry.slots
                );
            }
            entry.bound_credential_ids.push(credential_id);
            entry.last_rotated_at = Some(now);
        }
        self.save()
    }

    /// 解绑某代理上的某凭据；若代理不存在静默忽略
    pub fn unbind(&self, proxy_id: &str, credential_id: u64) -> anyhow::Result<()> {
        let changed = {
            let mut entries = self.entries.write();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == proxy_id) {
                let before = entry.bound_credential_ids.len();
                entry.bound_credential_ids.retain(|&id| id != credential_id);
                before != entry.bound_credential_ids.len()
            } else {
                false
            }
        };
        if changed {
            self.save()?;
        }
        Ok(())
    }

    /// 解绑某凭据在所有代理上的绑定（凭据被删除时调用）
    pub fn unbind_credential(&self, credential_id: u64) -> Vec<String> {
        let mut affected = Vec::new();
        {
            let mut entries = self.entries.write();
            for entry in entries.iter_mut() {
                let before = entry.bound_credential_ids.len();
                entry.bound_credential_ids.retain(|&id| id != credential_id);
                if before != entry.bound_credential_ids.len() {
                    affected.push(entry.id.clone());
                }
            }
        }
        if !affected.is_empty() {
            let _ = self.save();
        }
        affected
    }

    /// 批量加入新代理（去重：host:port 相同即视为重复，不论 scheme/用户名/密码）
    ///
    /// 用于交互式批量导入（管理员从 textarea 粘贴一批代理），需要拒绝重复以避免污染池。
    /// 错误信息会区分"批内重复"（与本批次已经新增的某行冲突）和"与历史重复"（命中池里已有条目），
    /// 便于用户排查粘贴失误。
    ///
    /// 返回：每条输入对应的结果（id 或 错误消息）
    pub fn add_many(&self, new_entries: Vec<ProxyEntry>) -> Vec<Result<String, String>> {
        let mut out = Vec::with_capacity(new_entries.len());
        {
            let mut entries = self.entries.write();
            // 历史已有 host:port → id 集合，预先建索引避免 O(N²) 扫描
            let existing_hp: HashMap<String, String> = entries
                .iter()
                .map(|e| (host_port_of(&e.url).to_string(), e.id.clone()))
                .collect();
            // 本批次已经成功入池的 host:port → 输入行号
            let mut batch_seen: HashMap<String, usize> = HashMap::new();
            for (idx, mut item) in new_entries.into_iter().enumerate() {
                let hp = host_port_of(&item.url).to_string();
                if let Some(prev_idx) = batch_seen.get(&hp) {
                    out.push(Err(format!(
                        "批内重复（与第 {} 行 host:port={} 冲突）",
                        prev_idx + 1,
                        hp
                    )));
                    continue;
                }
                if let Some(existing_id) = existing_hp.get(&hp) {
                    out.push(Err(format!(
                        "host:port 已存在: {}（既有代理 id={}）",
                        hp, existing_id
                    )));
                    continue;
                }
                if item.id.is_empty() {
                    item.id = generate_proxy_id();
                }
                if item.created_at.timestamp() == 0 {
                    item.created_at = Utc::now();
                }
                let id = item.id.clone();
                batch_seen.insert(hp, idx);
                entries.push(item);
                out.push(Ok(id));
            }
        }
        let _ = self.save();
        out
    }

    /// 批量加入新代理（**不去重**）：用于"跟随账号文件导入"等场景，
    /// 让每个凭据带的内嵌代理都各自落一条独立条目，避免多个凭据抢同一个 slot=1 的代理。
    ///
    /// 返回：每条输入对应的代理 id（空 id 会被自动生成）。
    pub fn add_many_force(&self, new_entries: Vec<ProxyEntry>) -> Vec<String> {
        let mut out = Vec::with_capacity(new_entries.len());
        {
            let mut entries = self.entries.write();
            for mut item in new_entries {
                if item.id.is_empty() {
                    item.id = generate_proxy_id();
                }
                if item.created_at.timestamp() == 0 {
                    item.created_at = Utc::now();
                }
                out.push(item.id.clone());
                entries.push(item);
            }
        }
        let _ = self.save();
        out
    }

    /// 删除单个代理；force=false 时若已绑定凭据则报错
    pub fn delete(&self, id: &str, force: bool) -> anyhow::Result<Vec<u64>> {
        let freed = {
            let mut entries = self.entries.write();
            let idx = entries
                .iter()
                .position(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("代理 {} 不存在", id))?;
            if !force && !entries[idx].bound_credential_ids.is_empty() {
                anyhow::bail!(
                    "代理 {} 仍绑定 {} 个凭据，请先解绑或使用 force=true",
                    id,
                    entries[idx].bound_credential_ids.len()
                );
            }
            let removed = entries.remove(idx);
            removed.bound_credential_ids
        };
        self.save()?;
        Ok(freed)
    }

    /// 修改代理槽位容量
    pub fn set_slots(&self, id: &str, slots: u32, force: bool) -> anyhow::Result<()> {
        if slots == 0 {
            anyhow::bail!("slots 必须 >= 1");
        }
        {
            let mut entries = self.entries.write();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("代理 {} 不存在", id))?;
            if slots < entry.used_slots() {
                if !force {
                    anyhow::bail!(
                        "新槽位数 {} 小于已绑定数 {}，请先解绑或使用 force=true",
                        slots,
                        entry.used_slots()
                    );
                }
                // force=true 时踢出超出部分（保留最早绑定的）
                entry.bound_credential_ids.truncate(slots as usize);
            }
            entry.slots = slots;
        }
        self.save()
    }

    /// 修改到期时间
    pub fn set_expires_at(&self, id: &str, expires_at: DateTime<Utc>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.write();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("代理 {} 不存在", id))?;
            entry.expires_at = expires_at;
        }
        self.save()
    }

    /// 后台轮换用：找到一个比当前代理"剩余有效期更长 + 仍有空槽 + 不是当前代理本身"的候选
    pub fn find_rotation_candidate(
        &self,
        current_proxy_id: &str,
        min_validity_hours: i64,
        excluded_credential: u64,
    ) -> Option<String> {
        let now = Utc::now();
        let entries = self.entries.read();
        entries
            .iter()
            .filter(|e| {
                e.id != current_proxy_id
                    && e.has_free_slot()
                    && (e.expires_at - now).num_hours() > min_validity_hours
                    && !e.bound_credential_ids.contains(&excluded_credential)
            })
            .max_by(|a, b| a.expires_at.cmp(&b.expires_at))
            .map(|e| e.id.clone())
    }

    /// 把绑定从 from_proxy 迁移到 to_proxy（同一凭据）
    pub fn migrate_binding(
        &self,
        credential_id: u64,
        from_proxy: &str,
        to_proxy: &str,
    ) -> anyhow::Result<()> {
        {
            let now = Utc::now();
            let mut entries = self.entries.write();

            // 校验目标代理可用
            let to_idx = entries
                .iter()
                .position(|e| e.id == to_proxy)
                .ok_or_else(|| anyhow::anyhow!("目标代理 {} 不存在", to_proxy))?;
            if !entries[to_idx].has_free_slot() {
                anyhow::bail!("目标代理 {} 已无空槽", to_proxy);
            }
            if entries[to_idx]
                .bound_credential_ids
                .contains(&credential_id)
            {
                anyhow::bail!("凭据 {} 已绑定到目标代理 {}", credential_id, to_proxy);
            }

            // 从源代理解绑
            if let Some(from) = entries.iter_mut().find(|e| e.id == from_proxy) {
                from.bound_credential_ids.retain(|&id| id != credential_id);
            }

            // 加到目标代理
            entries[to_idx].bound_credential_ids.push(credential_id);
            entries[to_idx].last_rotated_at = Some(now);
        }
        self.save()
    }

    // ============ 告警环形缓冲区 ============

    pub fn push_alert(&self, level: AlertLevel, message: impl Into<String>) {
        let alert = ProxyAlert {
            at: Utc::now(),
            level,
            message: message.into(),
        };
        let mut buf = self.alerts.write();
        if buf.len() >= ALERT_BUFFER_CAP {
            buf.pop_front();
        }
        buf.push_back(alert);
    }

    pub fn alerts(&self) -> Vec<ProxyAlert> {
        self.alerts.read().iter().cloned().collect()
    }

    /// 找到指定凭据当前绑定的代理 id（若有）
    pub fn find_binding_for(&self, credential_id: u64) -> Option<String> {
        self.entries
            .read()
            .iter()
            .find(|e| e.bound_credential_ids.contains(&credential_id))
            .map(|e| e.id.clone())
    }
}

/// 代理连通性测试结果
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub id: String,
    pub ok: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 通过给定代理测出口 IP + 延迟。
///
/// 试 https / http 两个 ipify 端点；任一成功即返回。
pub async fn test_proxy(
    entry: &ProxyEntry,
    tls_backend: crate::model::config::TlsBackend,
) -> ProxyTestResult {
    use std::time::Instant;
    let proxy = entry.to_proxy_config();
    let client = match crate::http_client::build_client(Some(&proxy), 10, tls_backend) {
        Ok(c) => c,
        Err(e) => {
            return ProxyTestResult {
                id: entry.id.clone(),
                ok: false,
                elapsed_ms: 0,
                ip: None,
                error: Some(format!("构建 HTTP client 失败: {}", e)),
            };
        }
    };

    let urls = [
        "https://api.ipify.org?format=json",
        "http://api.ipify.org?format=json",
    ];
    let start = Instant::now();
    let mut last_err = String::from("未知错误");
    for url in urls {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let elapsed = start.elapsed().as_millis() as u64;
                let body: serde_json::Value =
                    resp.json().await.unwrap_or(serde_json::Value::Null);
                let ip = body
                    .get("ip")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                return ProxyTestResult {
                    id: entry.id.clone(),
                    ok: true,
                    elapsed_ms: elapsed,
                    ip,
                    error: None,
                };
            }
            Ok(resp) => last_err = format!("HTTP {} from {}", resp.status(), url),
            Err(e) => last_err = format!("{}: {}", url, e),
        }
    }
    ProxyTestResult {
        id: entry.id.clone(),
        ok: false,
        elapsed_ms: start.elapsed().as_millis() as u64,
        ip: None,
        error: Some(last_err),
    }
}

/// 生成简短的代理 ID："p-" + 8 位 hex
fn generate_proxy_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let rand_part = fastrand::u32(..);
    format!("p-{:08x}", (nanos as u32) ^ rand_part)
}

/// 解析单行 host:port:user:pass 为 ProxyEntry（不含 expires_at / slots，由调用方填）
pub fn parse_host_port_user_pass_line(line: &str) -> anyhow::Result<(String, String, String)> {
    // host:port:user:pass —— 注意 user/pass 可能含 ':'，所以仅 split 前两段
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() != 4 {
        anyhow::bail!("行格式错误（期望 4 段冒号分隔）: {}", line);
    }
    let host = parts[0].trim();
    let port = parts[1].trim();
    let user = parts[2].trim();
    let pass = parts[3].trim();
    if host.is_empty() || port.is_empty() {
        anyhow::bail!("host 或 port 为空: {}", line);
    }
    let _: u16 = port.parse().context("port 必须是有效端口号")?;
    Ok((format!("{}:{}", host, port), user.to_string(), pass.to_string()))
}

/// 从 `scheme://host:port[/path?query]` 形式的代理 URL 中抠出 `host:port`。
///
/// 用于代理池去重判定：忽略 scheme（http/socks5/...）和用户名/密码，
/// 只看 IP/域名 + 端口。无法识别时返回原 URL（让等值比较自动退化为整 URL 比较）。
pub fn host_port_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    // 忽略路径和查询串
    let no_path = after_scheme
        .split_once('/')
        .map(|(hp, _)| hp)
        .unwrap_or(after_scheme);
    let no_query = no_path.split('?').next().unwrap_or(no_path);
    // URL 习惯上不会带 user:pass（add_many 入口已 split），保险起见再削一下
    no_query.rsplit_once('@').map(|(_, hp)| hp).unwrap_or(no_query)
}

/// 拼接代理 URL：scheme://host:port
pub fn build_proxy_url(scheme: &str, host_port: &str) -> String {
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "socks5" | "socks5h" => scheme.to_ascii_lowercase(),
        other => format!("invalid_{}", other),
    };
    format!("{}://{}", scheme, host_port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn fresh_entry(id: &str, slots: u32, hours_valid: i64) -> ProxyEntry {
        ProxyEntry {
            id: id.to_string(),
            url: format!("http://{}.example.com:1080", id),
            username: None,
            password: None,
            expires_at: Utc::now() + Duration::hours(hours_valid),
            slots,
            bound_credential_ids: vec![],
            label: None,
            created_at: Utc::now(),
            last_rotated_at: None,
        }
    }

    #[test]
    fn test_parse_line_basic() {
        let (hp, user, pass) =
            parse_host_port_user_pass_line("168.143.72.123:35123:JFkinzgf:LdSmlpgbv3").unwrap();
        assert_eq!(hp, "168.143.72.123:35123");
        assert_eq!(user, "JFkinzgf");
        assert_eq!(pass, "LdSmlpgbv3");
    }

    #[test]
    fn test_parse_line_password_with_colons() {
        let (hp, user, pass) = parse_host_port_user_pass_line("h:1234:u:p:a:s:s").unwrap();
        assert_eq!(hp, "h:1234");
        assert_eq!(user, "u");
        assert_eq!(pass, "p:a:s:s");
    }

    #[test]
    fn test_parse_line_bad_port() {
        let res = parse_host_port_user_pass_line("h:notport:u:p");
        assert!(res.is_err());
    }

    #[test]
    fn test_auto_bind_picks_widest_then_longest() {
        let pool = ProxyPool::empty();
        // p1: 1 slot 100h；p2: 2 slot 50h；p3: 2 slot 200h
        pool.entries
            .write()
            .extend([fresh_entry("p1", 1, 100), fresh_entry("p2", 2, 50), fresh_entry("p3", 2, 200)]);

        // 自动绑定凭据 1：剩余槽位 2 优先（p2、p3），其中到期更远的 p3 胜出
        let chosen = pool.auto_bind(1, 24).unwrap();
        assert_eq!(chosen, "p3");
    }

    #[test]
    fn test_pick_idle_candidate_skips_recent_failures() {
        let pool = ProxyPool::empty();
        pool.entries
            .write()
            .extend([fresh_entry("p1", 2, 200), fresh_entry("p2", 2, 100)]);

        // 默认会挑 p1（剩余槽相同，到期更远）
        assert_eq!(pool.pick_idle_candidate(24), Some("p1".to_string()));

        // 标记 p1 最近失败 → pick 应跳到 p2
        pool.mark_recent_failure("p1");
        assert_eq!(pool.pick_idle_candidate(24), Some("p2".to_string()));

        // 清除冷却 → 又能挑回 p1
        pool.clear_recent_failure("p1");
        assert_eq!(pool.pick_idle_candidate(24), Some("p1".to_string()));
    }

    #[test]
    fn test_auto_bind_skips_recent_failures() {
        let pool = ProxyPool::empty();
        pool.entries
            .write()
            .extend([fresh_entry("p1", 1, 200), fresh_entry("p2", 1, 100)]);
        pool.mark_recent_failure("p1");
        // p1 在冷却中 → auto_bind 必须挑 p2
        let chosen = pool.auto_bind(1, 24).unwrap();
        assert_eq!(chosen, "p2");
    }

    #[test]
    fn test_pick_idle_candidate_excluding_filters_both() {
        let pool = ProxyPool::empty();
        pool.entries.write().extend([
            fresh_entry("p1", 1, 200),
            fresh_entry("p2", 1, 150),
            fresh_entry("p3", 1, 100),
        ]);
        pool.mark_recent_failure("p2");
        let mut tried = std::collections::HashSet::new();
        tried.insert("p1".to_string());
        // p1 已试 + p2 冷却 → 只剩 p3
        assert_eq!(
            pool.pick_idle_candidate_excluding(24, &tried),
            Some("p3".to_string())
        );
    }

    #[test]
    fn test_auto_bind_skips_expiring() {
        let pool = ProxyPool::empty();
        // p1: 1 slot 12h（小于 24h 阈值），p2: 1 slot 48h
        pool.entries
            .write()
            .extend([fresh_entry("p1", 1, 12), fresh_entry("p2", 1, 48)]);
        let chosen = pool.auto_bind(1, 24).unwrap();
        assert_eq!(chosen, "p2");
    }

    #[test]
    fn test_auto_bind_fails_on_empty_pool() {
        let pool = ProxyPool::empty();
        assert!(pool.auto_bind(1, 24).is_err());
    }

    #[test]
    fn test_auto_bind_fails_when_all_full_or_expiring() {
        let pool = ProxyPool::empty();
        let mut p1 = fresh_entry("p1", 1, 100);
        p1.bound_credential_ids.push(99); // 已满
        let p2 = fresh_entry("p2", 1, 12); // 即将到期
        pool.entries.write().extend([p1, p2]);
        assert!(pool.auto_bind(1, 24).is_err());
    }

    #[test]
    fn test_manual_bind_idempotent() {
        let pool = ProxyPool::empty();
        pool.entries.write().push(fresh_entry("p1", 2, 100));
        pool.manual_bind("p1", 1).unwrap();
        pool.manual_bind("p1", 1).unwrap(); // 幂等不出错
        let entry = pool.get("p1").unwrap();
        assert_eq!(entry.bound_credential_ids, vec![1]);
    }

    #[test]
    fn test_manual_bind_full_slot() {
        let pool = ProxyPool::empty();
        let mut p = fresh_entry("p1", 1, 100);
        p.bound_credential_ids.push(99);
        pool.entries.write().push(p);
        let res = pool.manual_bind("p1", 1);
        assert!(res.is_err());
    }

    #[test]
    fn test_unbind_credential_clears_all() {
        let pool = ProxyPool::empty();
        let mut p1 = fresh_entry("p1", 2, 100);
        p1.bound_credential_ids.extend([1, 2]);
        let mut p2 = fresh_entry("p2", 2, 100);
        p2.bound_credential_ids.push(1);
        pool.entries.write().extend([p1, p2]);

        let affected = pool.unbind_credential(1);
        assert_eq!(affected.len(), 2);
        assert!(!pool.get("p1").unwrap().bound_credential_ids.contains(&1));
        assert!(!pool.get("p2").unwrap().bound_credential_ids.contains(&1));
    }

    #[test]
    fn test_set_slots_force_kicks_excess() {
        let pool = ProxyPool::empty();
        let mut p = fresh_entry("p1", 3, 100);
        p.bound_credential_ids.extend([10, 20, 30]);
        pool.entries.write().push(p);
        pool.set_slots("p1", 2, true).unwrap();
        let e = pool.get("p1").unwrap();
        assert_eq!(e.slots, 2);
        assert_eq!(e.bound_credential_ids, vec![10, 20]);
    }

    #[test]
    fn test_set_slots_no_force_blocks_shrink() {
        let pool = ProxyPool::empty();
        let mut p = fresh_entry("p1", 3, 100);
        p.bound_credential_ids.extend([10, 20, 30]);
        pool.entries.write().push(p);
        assert!(pool.set_slots("p1", 2, false).is_err());
    }

    #[test]
    fn test_find_rotation_candidate() {
        let pool = ProxyPool::empty();
        // 当前 p_old 12h；p1 100h 已满；p2 200h 1 槽空闲
        let mut p_old = fresh_entry("p_old", 1, 12);
        p_old.bound_credential_ids.push(7);
        let mut p1 = fresh_entry("p1", 1, 100);
        p1.bound_credential_ids.push(99);
        let p2 = fresh_entry("p2", 1, 200);
        pool.entries.write().extend([p_old, p1, p2]);

        let candidate = pool.find_rotation_candidate("p_old", 24, 7);
        assert_eq!(candidate, Some("p2".to_string()));
    }

    #[test]
    fn test_proxy_config_for_expired() {
        let pool = ProxyPool::empty();
        let mut p = fresh_entry("p1", 1, 100);
        p.expires_at = Utc::now() - Duration::hours(1);
        pool.entries.write().push(p);
        assert!(pool.proxy_config_for("p1").is_err());
    }

    fn make_entry(url: &str) -> ProxyEntry {
        ProxyEntry {
            id: String::new(),
            url: url.to_string(),
            username: None,
            password: None,
            expires_at: Utc::now() + Duration::hours(24),
            slots: 1,
            bound_credential_ids: vec![],
            label: None,
            created_at: Utc::now(),
            last_rotated_at: None,
        }
    }

    #[test]
    fn test_host_port_of_basic() {
        assert_eq!(host_port_of("http://1.2.3.4:8080"), "1.2.3.4:8080");
        assert_eq!(host_port_of("socks5://1.2.3.4:1080"), "1.2.3.4:1080");
        assert_eq!(host_port_of("https://example.com:443/path?x=1"), "example.com:443");
        // 没有 scheme 时退化为整串
        assert_eq!(host_port_of("1.2.3.4:8080"), "1.2.3.4:8080");
    }

    #[test]
    fn test_add_many_dedup_by_host_port_across_schemes() {
        // 同一 host:port，scheme 不同也算重复
        let pool = ProxyPool::empty();
        let res = pool.add_many(vec![
            make_entry("http://1.2.3.4:8080"),
            make_entry("socks5://1.2.3.4:8080"),
        ]);
        assert!(res[0].is_ok());
        assert!(res[1].is_err());
        assert!(res[1].as_ref().unwrap_err().contains("批内重复"));
        assert_eq!(pool.entries.read().len(), 1);
    }

    #[test]
    fn test_add_many_dedup_against_existing() {
        let pool = ProxyPool::empty();
        let _ = pool.add_many(vec![make_entry("http://1.2.3.4:8080")]);
        let res = pool.add_many(vec![make_entry("http://1.2.3.4:8080")]);
        assert!(res[0].is_err());
        let msg = res[0].as_ref().unwrap_err();
        assert!(msg.contains("host:port 已存在"));
    }

    #[test]
    fn test_add_many_force_allows_duplicates() {
        // 跟随账号文件导入：相同 host:port 也要能落多条独立记录
        let pool = ProxyPool::empty();
        let ids1 = pool.add_many_force(vec![make_entry("http://1.2.3.4:8080")]);
        let ids2 = pool.add_many_force(vec![make_entry("http://1.2.3.4:8080")]);
        assert_eq!(ids1.len(), 1);
        assert_eq!(ids2.len(), 1);
        assert_ne!(ids1[0], ids2[0]);
        assert_eq!(pool.entries.read().len(), 2);
    }

    #[test]
    fn test_alerts_ringbuffer() {
        let pool = ProxyPool::empty();
        for i in 0..120 {
            pool.push_alert(AlertLevel::Warn, format!("alert {}", i));
        }
        let alerts = pool.alerts();
        assert_eq!(alerts.len(), ALERT_BUFFER_CAP);
        // 最早的 0..20 已被淘汰
        assert!(alerts.first().unwrap().message.starts_with("alert 20"));
        assert_eq!(alerts.last().unwrap().message, "alert 119");
    }
}
