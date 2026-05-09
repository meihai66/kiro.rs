//! 凭据级实时指标：当前并发数 + 最近 60 秒 RPM
//!
//! - `InFlightCounter`：原子计数，请求开始 +1，结束 -1（RAII guard）
//! - `RpmTracker`：60 个秒桶环形数组，记录每个秒的请求次数；读取时累加最近 60s

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 60 秒环形桶 RPM 跟踪器
///
/// 每个桶存储 (秒级 epoch 时间戳, 该秒的请求计数)。
/// 桶下标 = `epoch_secs % 60`；读取时累加所有时间戳 > now-60 的桶。
pub struct RpmTracker {
    buckets: Mutex<[(u64, u32); 60]>,
}

impl Default for RpmTracker {
    fn default() -> Self {
        Self {
            buckets: Mutex::new([(0u64, 0u32); 60]),
        }
    }
}

impl RpmTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 记录一次请求
    pub fn record(&self) {
        let now = current_epoch_secs();
        let idx = (now % 60) as usize;
        let mut buckets = self.buckets.lock();
        if buckets[idx].0 != now {
            // 跨过 60s 边界，旧桶覆盖
            buckets[idx] = (now, 1);
        } else {
            buckets[idx].1 = buckets[idx].1.saturating_add(1);
        }
    }

    /// 最近 60 秒的请求总数
    pub fn rpm_60s(&self) -> u32 {
        let now = current_epoch_secs();
        let cutoff = now.saturating_sub(60);
        let buckets = self.buckets.lock();
        buckets
            .iter()
            .filter(|(ts, _)| *ts > cutoff)
            .map(|(_, c)| *c)
            .sum()
    }

    /// 取最近 60 秒每秒的请求数（用于细粒度调试，可选）
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<(u64, u32)> {
        let now = current_epoch_secs();
        let cutoff = now.saturating_sub(60);
        let buckets = self.buckets.lock();
        let mut result: Vec<(u64, u32)> = buckets
            .iter()
            .filter(|(ts, _)| *ts > cutoff)
            .copied()
            .collect();
        result.sort_by_key(|(ts, _)| *ts);
        result
    }
}

/// RAII guard：drop 时自动 dec。请求路径开始 inc，guard 由 drop 触发减计数。
pub struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    pub fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // saturating sub 防止极端情况溢出
        let prev = self.counter.fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            // 不可能发生；防御性回滚
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_in_flight_guard_inc_dec() {
        let c = Arc::new(AtomicU32::new(0));
        {
            let _g = InFlightGuard::new(c.clone());
            assert_eq!(c.load(Ordering::Relaxed), 1);
            {
                let _g2 = InFlightGuard::new(c.clone());
                assert_eq!(c.load(Ordering::Relaxed), 2);
            }
            assert_eq!(c.load(Ordering::Relaxed), 1);
        }
        assert_eq!(c.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_rpm_basic() {
        let t = RpmTracker::default();
        for _ in 0..10 {
            t.record();
        }
        assert_eq!(t.rpm_60s(), 10);
    }

    #[test]
    fn test_rpm_buckets_advance() {
        let t = RpmTracker::default();
        t.record();
        sleep(Duration::from_millis(1100));
        t.record();
        // 两次记录跨秒，仍都在 60s 窗口内
        assert_eq!(t.rpm_60s(), 2);
    }
}
