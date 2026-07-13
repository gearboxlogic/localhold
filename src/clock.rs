//! Injectable wall and monotonic time abstraction for deterministic tests.

#[cfg(any(test, feature = "testing"))]
use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, Ordering},
};
use std::{future::Future, pin::Pin, sync::OnceLock, time::Duration};

#[cfg(any(test, feature = "testing"))]
use chrono::TimeZone as _;
use chrono::{DateTime, Utc};

/// A boxed sleep future returned by [`Clock::sleep`].
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Marker returned when [`timeout`] reaches its clock-controlled deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Elapsed;

/// Run `future` until it completes or the injected clock reaches `duration`.
///
/// # Errors
///
/// Returns [`Elapsed`] when the clock-controlled deadline wins the race.
#[expect(clippy::integer_division_remainder_used, reason = "false positive from tokio::select! macro expansion")]
pub async fn timeout<T, F>(clock: &dyn Clock, duration: Duration, future: F) -> Result<T, Elapsed>
where
    F: Future<Output = T>,
{
    let sleep = clock.sleep(duration);
    tokio::pin!(sleep);
    tokio::pin!(future);
    tokio::select! {
        biased;
        output = &mut future => Ok(output),
        () = &mut sleep => Err(Elapsed),
    }
}

/// Injectable wall-clock and scheduler abstraction.
///
/// Production code uses [`SystemClock`]. Tests use [`MockClock`] to control
/// timestamp generation, elapsed-time decisions, and sleeping tasks together.
pub trait Clock: Send + Sync + std::fmt::Debug + 'static {
    /// Returns the current time according to this clock.
    fn now(&self) -> DateTime<Utc>;

    /// Returns monotonic elapsed time from an arbitrary clock-local origin.
    fn monotonic(&self) -> Duration;

    /// Completes after `duration` has elapsed according to this clock.
    fn sleep(&self, duration: Duration) -> Sleep;
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

    fn monotonic(&self) -> Duration {
        static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
        ORIGIN.get_or_init(std::time::Instant::now).elapsed()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Test clock with atomic millisecond resolution.
///
/// Allows pinning, advancing, and jumping time for deterministic tests.
#[cfg(any(test, feature = "testing"))]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MockClock {
    state: Arc<MockClockState>,
}

#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
struct MockClockState {
    millis_since_epoch: AtomicI64,
    monotonic_nanos: AtomicU64,
    advanced: tokio::sync::Notify,
}

#[cfg(any(test, feature = "testing"))]
impl MockClock {
    /// Creates a mock clock at the Unix epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(MockClockState {
                millis_since_epoch: AtomicI64::new(0),
                monotonic_nanos: AtomicU64::new(0),
                advanced: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Creates a mock clock pinned to a specific time.
    #[must_use]
    pub fn pinned(time: DateTime<Utc>) -> Self {
        Self {
            state: Arc::new(MockClockState {
                millis_since_epoch: AtomicI64::new(time.timestamp_millis()),
                monotonic_nanos: AtomicU64::new(0),
                advanced: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Advances the clock by `duration`.
    pub fn advance(&self, duration: chrono::TimeDelta) {
        let millis = duration.num_milliseconds();
        let _previous_wall = self.state.millis_since_epoch.fetch_add(millis, Ordering::AcqRel);
        if millis > 0 {
            let nanos = duration.num_nanoseconds().and_then(|value| u64::try_from(value).ok()).unwrap_or(u64::MAX);
            let _previous_monotonic = self
                .state
                .monotonic_nanos
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| Some(current.saturating_add(nanos)));
        }
        self.state.advanced.notify_waiters();
    }

    /// Jumps to an absolute time.
    pub fn set(&self, time: DateTime<Utc>) {
        self.state.millis_since_epoch.store(time.timestamp_millis(), Ordering::Release);
        self.state.advanced.notify_waiters();
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
        let millis = self.state.millis_since_epoch.load(Ordering::Acquire);
        #[expect(clippy::expect_used, reason = "MockClock always holds valid millis")]
        Utc.timestamp_millis_opt(millis).single().expect("MockClock millis should always be valid")
    }

    fn monotonic(&self) -> Duration {
        Duration::from_nanos(self.state.monotonic_nanos.load(Ordering::Acquire))
    }

    #[expect(clippy::excessive_nesting, reason = "register-before-check loop prevents lost mock-clock wakeups")]
    fn sleep(&self, duration: Duration) -> Sleep {
        let state = Arc::clone(&self.state);
        let duration_nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        let deadline = state.monotonic_nanos.load(Ordering::Acquire).saturating_add(duration_nanos);
        Box::pin(async move {
            loop {
                let notified = state.advanced.notified();
                tokio::pin!(notified);
                let _already_notified = notified.as_mut().enable();
                if state.monotonic_nanos.load(Ordering::Acquire) >= deadline {
                    return;
                }
                notified.await;
            }
        })
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

    #[tokio::test]
    async fn mock_clock_sleep_completes_only_after_advance() {
        let clock = MockClock::pinned(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
        let sleep = clock.sleep(Duration::from_secs(5));
        tokio::pin!(sleep);

        clock.advance(chrono::TimeDelta::seconds(4));
        assert!(futures::poll!(sleep.as_mut()).is_pending());
        clock.advance(chrono::TimeDelta::seconds(1));
        sleep.await;
    }

    #[tokio::test]
    async fn timeout_is_driven_by_mock_clock() {
        let clock = MockClock::new();
        let timed = timeout(&clock, Duration::from_secs(30), std::future::pending::<()>());
        tokio::pin!(timed);
        assert!(futures::poll!(timed.as_mut()).is_pending());

        clock.advance(chrono::TimeDelta::seconds(30));
        assert_eq!(timed.await, Err(Elapsed));
    }

    #[test]
    fn wall_clock_set_does_not_rewind_monotonic_time() {
        let clock = MockClock::new();
        clock.advance(chrono::TimeDelta::seconds(2));
        let elapsed = clock.monotonic();
        clock.set(Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(clock.monotonic(), elapsed);
    }
}
