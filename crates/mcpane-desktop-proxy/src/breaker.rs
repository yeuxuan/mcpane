//! 按供应商的熔断器：
//! 连续失败 N 次（默认 4，按 app 可调），或最近 20 次里失败率 ≥60%（至少 10 次样本）→ Open 冷却期（默认 60s）→
//! 到期进入 HalfOpen，同一时刻只放行一个探测 → 探测成功 2 次恢复 Closed，
//! HalfOpen 期间任一失败立即重新 Open。
//! 错误率用滑动窗口而非累计计数：间歇抖动的「半坏」站点不会被早年的成功稀释。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::router::RouteTuning;

const SUCCESS_THRESHOLD: u32 = 2;
const ERROR_RATE_WINDOW: usize = 20;
const ERROR_RATE_MIN_SAMPLES: usize = 10;
/// 失败率阈值（百分比）。
const ERROR_RATE_PERCENT: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
struct Breaker {
    consecutive_failures: u32,
    half_open_successes: u32,
    open_until: Option<Instant>,
    half_open: bool,
    /// HalfOpen 探测进行中：并发请求不再挤向同一个未确认恢复的供应商。
    probe_in_flight: bool,
    /// 最近的请求结果（true = 失败），错误率判定用。
    recent: VecDeque<bool>,
    last_error: Option<String>,
    /// Unix 毫秒（仅展示用）。
    last_failure_at: Option<u64>,
    /// 本次运行以来成功响应 / 失败的累计次数（展示转发分布用，重启归零）。
    served: u64,
    failed: u64,
}

impl Breaker {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            half_open_successes: 0,
            open_until: None,
            half_open: false,
            probe_in_flight: false,
            recent: VecDeque::new(),
            last_error: None,
            last_failure_at: None,
            served: 0,
            failed: 0,
        }
    }

    /// 读状态时顺带推进 Open → HalfOpen。
    fn state(&mut self, now: Instant) -> BreakerState {
        match self.open_until {
            Some(until) if now < until => BreakerState::Open,
            Some(_) => {
                self.open_until = None;
                self.half_open = true;
                self.half_open_successes = 0;
                self.probe_in_flight = false;
                BreakerState::HalfOpen
            }
            None if self.half_open => BreakerState::HalfOpen,
            None => BreakerState::Closed,
        }
    }

    fn push_outcome(&mut self, failed: bool) {
        if self.recent.len() == ERROR_RATE_WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(failed);
    }

    fn error_rate_tripped(&self) -> bool {
        let failures = self.recent.iter().filter(|failed| **failed).count();
        self.recent.len() >= ERROR_RATE_MIN_SAMPLES
            && failures * 100 >= self.recent.len() * ERROR_RATE_PERCENT
    }

    fn open(&mut self, now: Instant, cooldown: Duration) {
        self.open_until = Some(now + cooldown);
        self.half_open = false;
        self.probe_in_flight = false;
        self.recent.clear();
    }
}

/// 本次尝试能否发往该供应商。
pub(crate) enum Admission<'a> {
    /// 正常放行（Closed / Open 兜底）。
    Go,
    /// HalfOpen 探测名额：随尝试结束（含请求被取消）释放。
    Probe(ProbeGuard<'a>),
    /// HalfOpen 且已有探测在途：换下一个候选。
    Busy,
}

pub(crate) struct ProbeGuard<'a> {
    breakers: &'a Breakers,
    key: String,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.breakers.inner.lock()
            && let Some(breaker) = map.get_mut(&self.key)
        {
            breaker.probe_in_flight = false;
        }
    }
}

/// 熔断器快照（IPC 展示面）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakerSnapshot {
    pub state: BreakerState,
    pub consecutive_failures: u32,
    /// Open 剩余秒数（非 Open 为 None）。
    pub open_remaining_secs: Option<u64>,
    pub last_error: Option<String>,
    pub last_failure_at: Option<u64>,
    pub served: u64,
    pub failed: u64,
}

/// 全部熔断器；键为 `app:provider_id`，路由快照替换时保留（切换/编辑不清零健康史）。
#[derive(Default)]
pub(crate) struct Breakers {
    inner: Mutex<HashMap<String, Breaker>>,
}

impl Breakers {
    pub fn is_available(&self, key: &str) -> bool {
        let Ok(mut map) = self.inner.lock() else {
            return true;
        };
        map.get_mut(key)
            .is_none_or(|b| match b.state(Instant::now()) {
                BreakerState::Open => false,
                BreakerState::HalfOpen => !b.probe_in_flight,
                BreakerState::Closed => true,
            })
    }

    /// 发送前领取名额：HalfOpen 同一时刻只放一个探测。
    pub(crate) fn admit(&self, key: &str) -> Admission<'_> {
        let Ok(mut map) = self.inner.lock() else {
            return Admission::Go;
        };
        let Some(breaker) = map.get_mut(key) else {
            return Admission::Go;
        };
        if breaker.state(Instant::now()) != BreakerState::HalfOpen {
            return Admission::Go;
        }
        if breaker.probe_in_flight {
            return Admission::Busy;
        }
        breaker.probe_in_flight = true;
        Admission::Probe(ProbeGuard {
            breakers: self,
            key: key.to_owned(),
        })
    }

    pub fn record_success(&self, key: &str) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        let breaker = map.entry(key.to_owned()).or_insert_with(Breaker::new);
        breaker.served += 1;
        breaker.consecutive_failures = 0;
        breaker.push_outcome(false);
        if breaker.half_open {
            breaker.half_open_successes += 1;
            if breaker.half_open_successes >= SUCCESS_THRESHOLD {
                breaker.half_open = false;
                breaker.last_error = None;
            }
        }
    }

    /// `immediate`：连不上 / 首字节超时等强信号，立即熔断而不等累计阈值。
    pub fn record_failure(&self, key: &str, error: &str, immediate: bool, tuning: &RouteTuning) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        let now = Instant::now();
        let breaker = map.entry(key.to_owned()).or_insert_with(Breaker::new);
        let state = breaker.state(now);
        breaker.failed += 1;
        breaker.consecutive_failures += 1;
        breaker.push_outcome(true);
        breaker.last_error = Some(error.to_owned());
        breaker.last_failure_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        if immediate
            || state == BreakerState::HalfOpen
            || breaker.consecutive_failures >= tuning.failure_threshold.max(1)
            || breaker.error_rate_tripped()
        {
            breaker.open(now, tuning.cooldown);
        }
    }

    /// 手动恢复：清熔断与连续失败，保留累计次数。
    pub fn reset(&self, key: &str) {
        if let Ok(mut map) = self.inner.lock()
            && let Some(breaker) = map.get_mut(key)
        {
            *breaker = Breaker {
                served: breaker.served,
                failed: breaker.failed,
                ..Breaker::new()
            };
        }
    }

    pub fn snapshot(&self, key: &str) -> BreakerSnapshot {
        let now = Instant::now();
        let Ok(mut map) = self.inner.lock() else {
            return BreakerSnapshot::closed();
        };
        let Some(breaker) = map.get_mut(key) else {
            return BreakerSnapshot::closed();
        };
        let state = breaker.state(now);
        BreakerSnapshot {
            state,
            consecutive_failures: breaker.consecutive_failures,
            open_remaining_secs: breaker
                .open_until
                .filter(|_| state == BreakerState::Open)
                .map(|until| until.saturating_duration_since(now).as_secs().max(1)),
            last_error: breaker.last_error.clone(),
            last_failure_at: breaker.last_failure_at,
            served: breaker.served,
            failed: breaker.failed,
        }
    }
}

impl BreakerSnapshot {
    fn closed() -> Self {
        Self {
            state: BreakerState::Closed,
            consecutive_failures: 0,
            open_remaining_secs: None,
            last_error: None,
            last_failure_at: None,
            served: 0,
            failed: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const T: RouteTuning = RouteTuning {
        first_byte: Duration::from_secs(60),
        idle: Duration::from_secs(120),
        failure_threshold: FAILURE_THRESHOLD,
        cooldown: Duration::from_secs(60),
    };
    const FAILURE_THRESHOLD: u32 = 4;

    #[test]
    fn opens_after_threshold_and_half_open_recovers() {
        let breakers = Breakers::default();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            breakers.record_failure("k", "boom", false, &T);
        }
        assert!(breakers.is_available("k"));
        breakers.record_failure("k", "boom", false, &T);
        assert!(!breakers.is_available("k"));
        assert_eq!(breakers.snapshot("k").state, BreakerState::Open);

        // 模拟冷却到期。
        if let Ok(mut map) = breakers.inner.lock() {
            map.get_mut("k").unwrap().open_until = Some(Instant::now());
        }
        assert!(breakers.is_available("k"));
        assert_eq!(breakers.snapshot("k").state, BreakerState::HalfOpen);
        breakers.record_success("k");
        assert_eq!(breakers.snapshot("k").state, BreakerState::HalfOpen);
        breakers.record_success("k");
        assert_eq!(breakers.snapshot("k").state, BreakerState::Closed);
    }

    #[test]
    fn half_open_failure_reopens_immediately() {
        let breakers = Breakers::default();
        for _ in 0..FAILURE_THRESHOLD {
            breakers.record_failure("k", "boom", false, &T);
        }
        if let Ok(mut map) = breakers.inner.lock() {
            map.get_mut("k").unwrap().open_until = Some(Instant::now());
        }
        assert!(breakers.is_available("k"));
        breakers.record_failure("k", "again", false, &T);
        assert!(!breakers.is_available("k"));
        breakers.reset("k");
        assert!(breakers.is_available("k"));
        breakers.record_failure("k", "refused", true, &T);
        assert!(!breakers.is_available("k"), "强信号立即熔断");
        breakers.reset("k");
        let snapshot = breakers.snapshot("k");
        assert_eq!(
            (snapshot.served, snapshot.failed),
            (0, u64::from(FAILURE_THRESHOLD) + 2)
        );
        assert_eq!(snapshot.consecutive_failures, 0);
    }

    #[test]
    fn intermittent_failures_trip_on_error_rate() {
        let breakers = Breakers::default();
        // 失败-失败-成功交替：连续失败永远到不了 4，但失败率 2/3 超过阈值。
        for round in 0..ERROR_RATE_MIN_SAMPLES {
            if round % 3 == 2 {
                breakers.record_success("k");
            } else {
                breakers.record_failure("k", "flaky", false, &T);
            }
            if !breakers.is_available("k") {
                assert!(round + 1 >= ERROR_RATE_MIN_SAMPLES, "样本不足时不应熔断");
                return;
            }
        }
        panic!("间歇失败率 2/3 应触发熔断");
    }

    #[test]
    fn occasional_failures_do_not_trip() {
        let breakers = Breakers::default();
        for round in 0..ERROR_RATE_WINDOW * 3 {
            if round % 3 == 0 {
                breakers.record_failure("k", "blip", false, &T);
            } else {
                breakers.record_success("k");
            }
        }
        assert!(breakers.is_available("k"));
    }

    #[test]
    fn half_open_admits_a_single_probe_until_it_settles() {
        let breakers = Breakers::default();
        breakers.record_failure("k", "down", true, &T);
        if let Ok(mut map) = breakers.inner.lock() {
            map.get_mut("k").unwrap().open_until = Some(Instant::now());
        }
        let probe = breakers.admit("k");
        assert!(matches!(probe, Admission::Probe(_)));
        assert!(matches!(breakers.admit("k"), Admission::Busy));
        assert!(!breakers.is_available("k"), "探测在途时排序也应跳过");
        // 探测被取消（客户端断开）：名额随 guard 释放。
        drop(probe);
        assert!(breakers.is_available("k"));
        let probe = breakers.admit("k");
        breakers.record_success("k");
        drop(probe);
        assert!(matches!(breakers.admit("k"), Admission::Probe(_)));
        assert!(matches!(breakers.admit("unknown"), Admission::Go));
    }

    #[test]
    fn tuned_threshold_and_cooldown_apply() {
        let breakers = Breakers::default();
        let strict = RouteTuning {
            failure_threshold: 1,
            cooldown: Duration::from_secs(600),
            ..T
        };
        breakers.record_failure("k", "boom", false, &strict);
        let snapshot = breakers.snapshot("k");
        assert_eq!(snapshot.state, BreakerState::Open);
        assert!(snapshot.open_remaining_secs.unwrap() > 60, "冷却时长按调参");
    }
}
