//! 代理池后台轮换任务
//!
//! 周期性扫描代理池：
//! - 对剩余有效期 < `warning_hours` 的绑定，尝试迁移到时间充足的备用代理
//! - 对已过期的代理，清绑相关凭据并标记 ProxyExhausted 冷却
//! - 找不到候选时记入告警环形缓冲区（不强制切换、不回退本地）

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::task::JoinHandle;

use crate::kiro::proxy_pool::{AlertLevel, ProxyPool};
use crate::kiro::token_manager::MultiTokenManager;

/// 启动后台代理轮换任务
///
/// # 参数
/// - `pool` 代理池
/// - `manager` token 管理器（用于读取凭据绑定 + 更新槽位）
/// - `warning_hours` 提前轮换阈值（小时）
/// - `interval` 扫描间隔
///
/// 返回 JoinHandle（程序退出时由 tokio runtime 清理）
pub fn start_rotation_task(
    pool: Arc<ProxyPool>,
    manager: Arc<MultiTokenManager>,
    warning_hours: i64,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!(
            warning_hours,
            interval_secs = interval.as_secs(),
            "代理池后台轮换任务已启动"
        );
        loop {
            tokio::time::sleep(interval).await;
            run_one_round(&pool, &manager, warning_hours).await;
        }
    })
}

/// 单次扫描；公开是为了 Admin API "立即轮换" 也能调用
pub async fn run_one_round(
    pool: &Arc<ProxyPool>,
    manager: &Arc<MultiTokenManager>,
    warning_hours: i64,
) {
    let now = Utc::now();
    let bindings = manager.list_credential_proxy_bindings();

    if bindings.is_empty() {
        tracing::debug!("代理池轮换：当前无凭据绑定，跳过本轮");
        return;
    }

    for (cred_id, slot_id) in bindings {
        let entry = match pool.get(&slot_id) {
            Some(e) => e,
            None => {
                // 槽位被删但凭据还指向，是一致性事故 → 清掉凭据 slot 并冷却
                tracing::warn!(
                    credential_id = cred_id,
                    slot_id = %slot_id,
                    "代理池轮换：凭据所指代理槽不存在，清理绑定"
                );
                pool.push_alert(
                    AlertLevel::Error,
                    format!(
                        "凭据 #{} 所指代理槽 {} 不存在，已清理绑定",
                        cred_id, slot_id
                    ),
                );
                let _ = manager.set_proxy_slot(cred_id, None);
                manager.report_proxy_exhausted(cred_id);
                continue;
            }
        };

        // 已过期：清绑 + 冷却
        if entry.is_expired(now) {
            tracing::warn!(
                credential_id = cred_id,
                slot_id = %slot_id,
                "代理池轮换：代理已过期，清绑凭据并标记 ProxyExhausted"
            );
            pool.push_alert(
                AlertLevel::Error,
                format!(
                    "代理 {} 已过期（凭据 #{}），已清绑；请补充代理或手动重新分配",
                    slot_id, cred_id
                ),
            );
            let _ = pool.unbind(&slot_id, cred_id);
            let _ = manager.set_proxy_slot(cred_id, None);
            manager.report_proxy_exhausted(cred_id);
            continue;
        }

        // 剩余有效期 < warning_hours：找候选
        let remaining_hours = (entry.expires_at - now).num_hours();
        if remaining_hours >= warning_hours {
            continue; // 时间充足，无需动作
        }

        match pool.find_rotation_candidate(&slot_id, warning_hours, cred_id) {
            Some(new_slot) => {
                tracing::info!(
                    credential_id = cred_id,
                    from = %slot_id,
                    to = %new_slot,
                    remaining_hours,
                    "代理池轮换：迁移凭据到剩余时间更长的代理"
                );
                if let Err(e) = pool.migrate_binding(cred_id, &slot_id, &new_slot) {
                    tracing::warn!(credential_id = cred_id, "代理池轮换：迁移失败: {}", e);
                    pool.push_alert(
                        AlertLevel::Error,
                        format!("凭据 #{} 代理迁移失败: {}", cred_id, e),
                    );
                    continue;
                }
                if let Err(e) = manager.set_proxy_slot(cred_id, Some(new_slot.clone())) {
                    tracing::warn!(
                        credential_id = cred_id,
                        "代理池轮换：更新凭据 proxy_slot_id 失败: {}",
                        e
                    );
                    // 凭据写不回，回滚池里的迁移避免不一致
                    let _ = pool.migrate_binding(cred_id, &new_slot, &slot_id);
                    continue;
                }
                pool.push_alert(
                    AlertLevel::Info,
                    format!("凭据 #{} 已从代理 {} 迁移到 {}", cred_id, slot_id, new_slot),
                );
            }
            None => {
                tracing::warn!(
                    credential_id = cred_id,
                    slot_id = %slot_id,
                    remaining_hours,
                    "代理池轮换：无可用备件代理（保持当前绑定）"
                );
                pool.push_alert(
                    AlertLevel::Warn,
                    format!(
                        "凭据 #{} 所绑代理 {} 剩余 {} 小时，但池中无可用备件",
                        cred_id, slot_id, remaining_hours
                    ),
                );
            }
        }
    }
}
