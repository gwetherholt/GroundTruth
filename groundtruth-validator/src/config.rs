use chrono::Duration;
use std::collections::HashMap;
use std::ops::RangeInclusive;

/// Validation thresholds and expected behavior for a single metric.
///
/// A metric is the logical "thing being measured" (e.g. `moisture`,
/// `temperature`). The validator looks up the matching `MetricConfig`
/// by metric name on every reading; if no config exists, the reading
/// passes through without metric-specific checks.
#[derive(Debug, Clone)]
pub struct MetricConfig {
    /// Inclusive range of plausible values. Outside → `Invalid`.
    pub valid_range: RangeInclusive<f64>,

    /// Inclusive range of plausible raw (e.g. ADC) values, if a raw
    /// reading is provided. Outside → `Invalid`. `None` skips the check.
    pub raw_valid_range: Option<RangeInclusive<i64>>,

    /// Max absolute delta between consecutive readings within
    /// `rate_window`. Larger → `Suspect`. Use [`f64::INFINITY`] to
    /// disable the rule.
    pub max_rate_of_change: f64,

    /// Window over which rate-of-change applies. If the previous reading
    /// is older than this, rate-of-change is not checked (large slow
    /// drifts are caught by Tier-2 drift, not by this rule).
    pub rate_window: Duration,

    /// Two consecutive readings within `stuck_threshold` of each other
    /// count as a "match" toward the stuck-detection streak.
    pub stuck_threshold: f64,

    /// A streak of this many matches → `Suspect`. The buffered history
    /// must hold at least `stuck_count - 1` prior readings.
    pub stuck_count: usize,

    /// How often the source is expected to publish, used by the Tier-2
    /// cadence signal. Falling far below this rate degrades health.
    pub expected_cadence: Duration,
}

impl MetricConfig {
    /// Build a config with sensible defaults around `valid_range`.
    /// Other fields can be overridden with the `with_*` setters.
    pub fn new(valid_range: RangeInclusive<f64>) -> Self {
        Self {
            valid_range,
            raw_valid_range: None,
            max_rate_of_change: f64::INFINITY,
            rate_window: Duration::seconds(600),
            stuck_threshold: 0.01,
            stuck_count: 6,
            expected_cadence: Duration::seconds(60),
        }
    }

    pub fn with_raw_range(mut self, range: RangeInclusive<i64>) -> Self {
        self.raw_valid_range = Some(range);
        self
    }

    pub fn with_max_rate_of_change(mut self, delta: f64) -> Self {
        self.max_rate_of_change = delta;
        self
    }

    pub fn with_rate_window(mut self, window: Duration) -> Self {
        self.rate_window = window;
        self
    }

    pub fn with_stuck_threshold(mut self, threshold: f64) -> Self {
        self.stuck_threshold = threshold;
        self
    }

    pub fn with_stuck_count(mut self, count: usize) -> Self {
        self.stuck_count = count;
        self
    }

    pub fn with_expected_cadence(mut self, cadence: Duration) -> Self {
        self.expected_cadence = cadence;
        self
    }
}

/// Top-level validator configuration. Holds per-metric configs plus
/// global Tier-2 / quarantine knobs. Build via [`ValidatorConfig::builder`].
#[derive(Debug, Clone)]
pub struct ValidatorConfig {
    pub metrics: HashMap<String, MetricConfig>,
    pub baseline_window: Duration,
    pub health_check_interval: Duration,
    pub quarantine_bad_threshold: f64,
    pub quarantine_recovery_threshold: f64,
    pub quarantine_consecutive_required: u32,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            metrics: HashMap::new(),
            baseline_window: Duration::days(7),
            health_check_interval: Duration::seconds(30),
            quarantine_bad_threshold: 40.0,
            quarantine_recovery_threshold: 70.0,
            quarantine_consecutive_required: 3,
        }
    }
}

impl ValidatorConfig {
    pub fn builder() -> ValidatorConfigBuilder {
        ValidatorConfigBuilder::default()
    }

    pub fn metric(&self, name: &str) -> Option<&MetricConfig> {
        self.metrics.get(name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidatorConfigBuilder {
    inner: ValidatorConfig,
}

impl ValidatorConfigBuilder {
    pub fn metric(mut self, name: impl Into<String>, config: MetricConfig) -> Self {
        self.inner.metrics.insert(name.into(), config);
        self
    }

    pub fn baseline_window(mut self, window: Duration) -> Self {
        self.inner.baseline_window = window;
        self
    }

    pub fn health_check_interval(mut self, interval: Duration) -> Self {
        self.inner.health_check_interval = interval;
        self
    }

    pub fn quarantine_bad_threshold(mut self, threshold: f64) -> Self {
        self.inner.quarantine_bad_threshold = threshold;
        self
    }

    pub fn quarantine_recovery_threshold(mut self, threshold: f64) -> Self {
        self.inner.quarantine_recovery_threshold = threshold;
        self
    }

    pub fn quarantine_consecutive_required(mut self, count: u32) -> Self {
        self.inner.quarantine_consecutive_required = count;
        self
    }

    pub fn build(self) -> ValidatorConfig {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sets_defaults() {
        let cfg = ValidatorConfig::builder().build();
        assert_eq!(cfg.quarantine_bad_threshold, 40.0);
        assert_eq!(cfg.quarantine_recovery_threshold, 70.0);
        assert_eq!(cfg.quarantine_consecutive_required, 3);
        assert_eq!(cfg.baseline_window, Duration::days(7));
        assert_eq!(cfg.health_check_interval, Duration::seconds(30));
        assert!(cfg.metrics.is_empty());
    }

    #[test]
    fn builder_overrides_thresholds() {
        let cfg = ValidatorConfig::builder()
            .quarantine_bad_threshold(20.0)
            .quarantine_recovery_threshold(80.0)
            .quarantine_consecutive_required(5)
            .baseline_window(Duration::days(3))
            .build();
        assert_eq!(cfg.quarantine_bad_threshold, 20.0);
        assert_eq!(cfg.quarantine_recovery_threshold, 80.0);
        assert_eq!(cfg.quarantine_consecutive_required, 5);
        assert_eq!(cfg.baseline_window, Duration::days(3));
    }

    #[test]
    fn builder_adds_metrics() {
        let cfg = ValidatorConfig::builder()
            .metric("moisture", MetricConfig::new(0.0..=100.0))
            .metric(
                "temperature",
                MetricConfig::new(-40.0..=200.0).with_max_rate_of_change(20.0),
            )
            .build();
        assert!(cfg.metric("moisture").is_some());
        assert!(cfg.metric("humidity").is_none());
        assert_eq!(cfg.metric("temperature").unwrap().max_rate_of_change, 20.0);
    }

    #[test]
    fn metric_config_defaults() {
        let m = MetricConfig::new(0.0..=100.0);
        assert_eq!(m.stuck_threshold, 0.01);
        assert_eq!(m.stuck_count, 6);
        assert!(m.raw_valid_range.is_none());
        assert!(m.max_rate_of_change.is_infinite());
    }

    #[test]
    fn metric_config_with_raw_range() {
        let m = MetricConfig::new(0.0..=100.0).with_raw_range(100..=3995);
        assert_eq!(m.raw_valid_range, Some(100..=3995));
    }
}
