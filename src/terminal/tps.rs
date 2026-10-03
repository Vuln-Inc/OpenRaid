//! Elapsed-time-weighted output throughput over the last minute.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Throughput {
    samples: VecDeque<(Instant, u64)>,
}

impl Throughput {
    /// Start with a baseline, not historical tokens from before console attachment.
    /// During the first minute use the observed duration; thereafter use 60 seconds.
    pub(super) fn sample(&mut self, now: Instant, tokens: u64) -> Option<f64> {
        if let Some(&(previous, previous_tokens)) = self.samples.back() {
            let elapsed = now.checked_duration_since(previous)?;
            if tokens < previous_tokens {
                self.samples.clear();
            } else if elapsed < SAMPLE_INTERVAL {
                return None;
            }
        }
        self.samples.push_back((now, tokens));
        let cutoff = now.checked_sub(WINDOW).unwrap_or(now);
        // Keep one baseline preceding the window for partial-interval weighting.
        while self.samples.len() > 1 && self.samples[1].0 <= cutoff {
            self.samples.pop_front();
        }
        let &(start, baseline) = self.samples.front().unwrap();
        let duration = now.duration_since(start).min(WINDOW).as_secs_f64();
        if duration == 0.0 {
            return Some(0.0);
        }
        let mut output = tokens.saturating_sub(baseline) as f64;
        if start < cutoff {
            let &(end, next_tokens) = &self.samples[1];
            let fraction = cutoff.duration_since(start).as_secs_f64()
                / end.duration_since(start).as_secs_f64();
            output -= next_tokens.saturating_sub(baseline) as f64 * fraction;
        }
        Some(output.max(0.0) / duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rate(rate: Option<f64>, expected: f64) {
        assert!((rate.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn baseline_excludes_historical_tokens_and_throttles_redraws() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        assert_rate(rate.sample(now, 90_000), 0.0);
        assert_eq!(rate.sample(now + Duration::from_millis(100), 90_005), None);
        assert_rate(rate.sample(now + Duration::from_secs(2), 90_020), 10.0);
    }

    #[test]
    fn irregular_samples_are_elapsed_weighted_not_rate_averaged() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        rate.sample(now, 0);
        assert_rate(rate.sample(now + Duration::from_secs(1), 100), 100.0);
        assert_rate(rate.sample(now + Duration::from_secs(10), 100), 10.0);
        assert_rate(rate.sample(now + Duration::from_secs(60), 600), 10.0);
    }

    #[test]
    fn completion_bursts_are_smoothed_and_expire_after_one_minute() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        rate.sample(now, 0);
        for second in 1..60 {
            rate.sample(now + Duration::from_secs(second), 0);
        }
        assert_rate(rate.sample(now + Duration::from_secs(60), 6_000), 100.0);
        assert_rate(rate.sample(now + Duration::from_secs(119), 6_000), 100.0);
        assert_rate(rate.sample(now + Duration::from_secs(120), 6_000), 0.0);
    }

    #[test]
    fn partial_interval_at_window_boundary_is_weighted() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        rate.sample(now, 0);
        rate.sample(now + Duration::from_secs(10), 100);
        assert_rate(rate.sample(now + Duration::from_secs(65), 650), 10.0);
        assert_rate(rate.sample(now + Duration::from_secs(125), 650), 0.0);
    }

    #[test]
    fn counter_reset_rebaselines_without_underflow_or_stale_history() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        rate.sample(now, 100);
        rate.sample(now + Duration::from_secs(2), 120);
        assert_rate(rate.sample(now + Duration::from_secs(3), 0), 0.0);
        assert_rate(rate.sample(now + Duration::from_secs(5), 20), 10.0);
    }

    #[test]
    fn sample_history_stays_bounded() {
        let now = Instant::now();
        let mut rate = Throughput::default();
        for second in 0..10_000 {
            rate.sample(now + Duration::from_secs(second), second * 10);
        }
        assert!(rate.samples.len() <= 61);
    }
}
