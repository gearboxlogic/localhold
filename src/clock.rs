//! Injectable wall-clock abstraction for deterministic time in tests.

#[cfg(any(test, feature = "testing"))]
use std::sync::atomic::{AtomicI64, Ordering};

#[cfg(any(test, feature = "testing"))]
use chrono::TimeZone as _;
use chrono::{DateTime, Utc};

/// Injectable wall-clock abstraction.
///
/// Production code uses [`SystemClock`] (delegates to `Utc::now()`).
/// Tests use [`MockClock`] to control time deterministically.
pub trait Clock: Send + Sync + std::fmt::Debug + 'static {
    /// Returns the current time according to this clock.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — delegates to `chrono::Utc::now()`.
#[derive(Debug)]
#[non_exhaustive]
pub struct SystemClock;

impl SystemClock {
    /// Creates a new system clock.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock with atomic millisecond resolution.
///
/// Allows pinning, advancing, and jumping time for deterministic tests.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
#[non_exhaustive]
pub struct MockClock {
    millis_since_epoch: AtomicI64,
}

#[cfg(any(test, feature = "testing"))]
impl MockClock {
    /// Creates a mock clock starting at the current real time.
    #[must_use]
    pub fn new() -> Self {
        Self {
            millis_since_epoch: AtomicI64::new(Utc::now().timestamp_millis()),
        }
    }

    /// Creates a mock clock pinned to a specific time.
    #[must_use]
    pub const fn pinned(time: DateTime<Utc>) -> Self {
        Self {
            millis_since_epoch: AtomicI64::new(time.timestamp_millis()),
        }
    }

    /// Advances the clock by `duration`.
    pub fn advance(&self, duration: chrono::TimeDelta) {
        let _prev = self.millis_since_epoch.fetch_add(duration.num_milliseconds(), Ordering::Relaxed);
    }

    /// Jumps to an absolute time.
    pub fn set(&self, time: DateTime<Utc>) {
        self.millis_since_epoch.store(time.timestamp_millis(), Ordering::Relaxed);
    }
}

#[cfg(any(test, feature = "testing"))]
impl Default for MockClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        let millis = self.millis_since_epoch.load(Ordering::Relaxed);
        #[expect(clippy::expect_used, reason = "MockClock always holds valid millis")]
        Utc.timestamp_millis_opt(millis).single().expect("MockClock millis should always be valid")
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn system_clock_returns_recent_time() {
        let clock = SystemClock;
        let now = clock.now();
        let real = Utc::now();
        let diff = (real - now).num_seconds().abs();
        assert!(diff < 2, "SystemClock should return near-current time");
    }

    #[test]
    fn mock_clock_pinned() {
        let t = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        let clock = MockClock::pinned(t);
        assert_eq!(clock.now(), t, "pinned clock should return the pinned time");
    }

    #[test]
    fn mock_clock_advance() {
        let t = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        let clock = MockClock::pinned(t);
        clock.advance(chrono::TimeDelta::hours(2));
        let expected = t + chrono::TimeDelta::hours(2);
        assert_eq!(clock.now(), expected, "advanced clock should match expected time");
    }

    #[test]
    fn mock_clock_set() {
        let clock = MockClock::new();
        let target = Utc.with_ymd_and_hms(2030, 12, 25, 0, 0, 0).unwrap();
        clock.set(target);
        assert_eq!(clock.now(), target, "set clock should match target time");
    }

    #[test]
    fn mock_clock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockClock>();
    }
}
