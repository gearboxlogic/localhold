//! Retry-delay policy for transient and rate-limited embedding requests.

use std::time::Duration;

const JITTER_MIN_PER_MILLE: u32 = 800;
const JITTER_MAX_PER_MILLE: u32 = 1_200;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl RetryPolicy {
    pub(crate) const fn new(max_retries: u32, initial_backoff: Duration, max_backoff: Duration) -> Self {
        Self {
            max_retries,
            initial_backoff,
            max_backoff,
        }
    }

    pub(crate) const fn max_retries(&self) -> u32 {
        self.max_retries
    }

    pub(crate) const fn max_backoff(&self) -> Duration {
        self.max_backoff
    }

    pub(crate) fn delay(&self, retry_index: u32, retry_after: Option<Duration>) -> Option<Duration> {
        let jitter = fastrand::u32(JITTER_MIN_PER_MILLE..=JITTER_MAX_PER_MILLE);
        self.delay_with_jitter(retry_index, retry_after, jitter)
    }

    fn delay_with_jitter(&self, retry_index: u32, retry_after: Option<Duration>, jitter_per_mille: u32) -> Option<Duration> {
        if retry_after.is_some_and(|delay| delay > self.max_backoff) {
            return None;
        }

        let multiplier = 2_u32.saturating_pow(retry_index);
        let exponential = self.initial_backoff.saturating_mul(multiplier).min(self.max_backoff);
        let jittered = scale_duration(exponential, jitter_per_mille).min(self.max_backoff);
        Some(retry_after.map_or(jittered, |provider_delay| provider_delay.max(jittered)))
    }
}

#[expect(
    clippy::integer_division,
    clippy::integer_division_remainder_used,
    reason = "integer millisecond scaling is bounded and intentionally rounds down"
)]
fn scale_duration(duration: Duration, per_mille: u32) -> Duration {
    let millis = duration.as_millis().saturating_mul(u128::from(per_mille)) / 1_000;
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::RetryPolicy;

    fn policy() -> RetryPolicy {
        RetryPolicy::new(2, Duration::from_millis(500), Duration::from_secs(30))
    }

    #[test]
    fn exponential_delay_uses_bounded_jitter() {
        let policy = policy();
        assert_eq!(policy.delay_with_jitter(0, None, 800), Some(Duration::from_millis(400)));
        assert_eq!(policy.delay_with_jitter(1, None, 1_200), Some(Duration::from_millis(1_200)));
    }

    #[test]
    fn provider_delay_is_a_minimum() {
        let delay = policy().delay_with_jitter(0, Some(Duration::from_secs(4)), 800);
        assert_eq!(delay, Some(Duration::from_secs(4)));
    }

    #[test]
    fn provider_delay_over_cap_skips_retry() {
        let delay = policy().delay_with_jitter(0, Some(Duration::from_secs(31)), 1_000);
        assert_eq!(delay, None);
    }

    #[test]
    fn exponential_delay_is_capped() {
        let delay = policy().delay_with_jitter(20, None, 1_200);
        assert_eq!(delay, Some(Duration::from_secs(30)));
    }
}
