use chrono::Duration;
use std::collections::HashMap;
use std::ops::RangeInclusive;

/// Default window for [`StuckMode::Duration`]: how long a quantized
/// value may sit unchanged before it looks frozen rather than stable.
pub const DEFAULT_STUCK_WINDOW_MINUTES: i64 = 60;

/// Default quantization step assumed for metrics that get
/// [`StuckMode::Duration`] via [`MetricConfig::with_stuck_defaults_for`].
/// 0.1 matches a DHT22's reported resolution for both °F and %RH.
pub const DEFAULT_RESOLUTION: f64 = 0.1;

/// How stuck-value detection decides a stream has frozen.
///
/// The two modes exist because "the value didn't move" means different
/// things depending on the transducer:
///
/// - A raw analog source (soil moisture, raw ADC) has a noise floor
///   much wider than its reporting precision. Six consecutive readings
///   landing within ±0.01 of each other is physically implausible, so
///   *counting* identical readings is a sound stuck signal.
/// - A quantized source (a DHT22 reports in 0.1° steps) that measures a
///   slow-moving quantity spends most of its life reporting the same
///   number. Identical consecutive readings are its normal state, so
///   counting them flags healthy sensors. What is abnormal is the value
///   not moving *for a long time*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StuckMode {
    /// `stuck_count` consecutive readings all within `stuck_threshold`
    /// of the newest one → Suspect. Right for noisy analog metrics.
    Count,
    /// The value has stayed within `resolution` of where it was for at
    /// least `stuck_window` → Suspect. Right for quantized, slow-moving
    /// metrics, where cadence is irrelevant to whether it's stuck.
    Duration,
}

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

    /// Which stuck-detection rule applies to this metric. See
    /// [`StuckMode`] for why this is per-metric.
    pub stuck_mode: StuckMode,

    /// [`StuckMode::Count`] only: two consecutive readings within
    /// `stuck_threshold` of each other count as a "match" toward the
    /// stuck-detection streak. Set it to the source's *noise floor* —
    /// the spread you'd expect a working sensor to produce at rest.
    pub stuck_threshold: f64,

    /// [`StuckMode::Count`] only: a streak of this many matches →
    /// `Suspect`. The buffered history must hold at least
    /// `stuck_count - 1` prior readings.
    pub stuck_count: usize,

    /// [`StuckMode::Duration`] only: the source's quantization step —
    /// the smallest change it is capable of reporting. Two values
    /// within this of each other are the same reading as far as the
    /// sensor is concerned. `None` falls back to `stuck_threshold`.
    pub resolution: Option<f64>,

    /// [`StuckMode::Duration`] only: how long the value must stay
    /// within `resolution` before the stream is called stuck.
    pub stuck_window: Duration,

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
            stuck_mode: StuckMode::Count,
            stuck_threshold: 0.01,
            stuck_count: 6,
            resolution: None,
            stuck_window: Duration::minutes(DEFAULT_STUCK_WINDOW_MINUTES),
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

    /// Turn the rate-of-change rule off for this metric. Use it where
    /// large steps between consecutive readings are the *expected*
    /// behavior of the setup rather than a symptom — a bench fixture
    /// whose probe is pulled out of one medium and pushed into another
    /// between readings, for instance.
    pub fn without_rate_check(mut self) -> Self {
        self.max_rate_of_change = f64::INFINITY;
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

    pub fn with_stuck_mode(mut self, mode: StuckMode) -> Self {
        self.stuck_mode = mode;
        self
    }

    pub fn with_resolution(mut self, resolution: f64) -> Self {
        self.resolution = Some(resolution);
        self
    }

    pub fn with_stuck_window(mut self, window: Duration) -> Self {
        self.stuck_window = window;
        self
    }

    /// Switch to [`StuckMode::Duration`] and set both of its
    /// parameters at once: `resolution` is the sensor's quantization
    /// step, `window` how long it may sit there before it's suspect.
    ///
    /// ```
    /// # use groundtruth_validator::{MetricConfig, StuckMode};
    /// # use chrono::Duration;
    /// // A DHT22: 0.1 °F steps, and a garage that moves ~1 °F/hour.
    /// let temperature = MetricConfig::new(-40.0..=200.0)
    ///     .with_time_based_stuck(0.1, Duration::minutes(60));
    /// assert_eq!(temperature.stuck_mode, StuckMode::Duration);
    /// assert_eq!(temperature.stuck_tolerance(), 0.1);
    /// ```
    pub fn with_time_based_stuck(mut self, resolution: f64, window: Duration) -> Self {
        self.stuck_mode = StuckMode::Duration;
        self.resolution = Some(resolution);
        self.stuck_window = window;
        self
    }

    /// Apply the stuck-detection profile that suits `metric`'s usual
    /// transducer family, leaving every other field alone.
    ///
    /// This is the one deliberately opinionated corner of the crate: it
    /// maps well-known metric names onto a mode so that callers (the
    /// server, the CLI) agree on defaults without duplicating the
    /// table. Metrics it doesn't recognize keep [`StuckMode::Count`].
    ///
    /// | Metric                    | Mode       | Why                                            |
    /// |---------------------------|------------|------------------------------------------------|
    /// | `temperature`, `humidity` | `Duration` | Quantized (0.1 steps), slow-moving             |
    /// | anything else             | `Count`    | Assumed noisy analog; repeats are implausible  |
    pub fn with_stuck_defaults_for(self, metric: &str) -> Self {
        if is_quantized_metric(metric) {
            self.with_time_based_stuck(
                DEFAULT_RESOLUTION,
                Duration::minutes(DEFAULT_STUCK_WINDOW_MINUTES),
            )
        } else {
            self
        }
    }

    /// The "values this close are the same value" tolerance for the
    /// active stuck mode: `resolution` when set, else `stuck_threshold`.
    pub fn stuck_tolerance(&self) -> f64 {
        match self.stuck_mode {
            StuckMode::Count => self.stuck_threshold,
            StuckMode::Duration => self.resolution.unwrap_or(self.stuck_threshold),
        }
    }

    /// How many prior readings Tier-1 needs buffered. Count-based
    /// stuck detection is the only rule that wants more than one.
    pub fn history_cap(&self) -> usize {
        self.stuck_count.saturating_sub(1).max(1)
    }
}

/// Whether `metric` names a quantized, slow-moving quantity whose
/// sensors normally repeat readings. Used by
/// [`MetricConfig::with_stuck_defaults_for`].
pub fn is_quantized_metric(metric: &str) -> bool {
    matches!(
        metric.to_ascii_lowercase().as_str(),
        "temperature" | "humidity"
    )
}

/// How much of the pipeline applies to a group of streams.
///
/// Tier-2 health scoring and Tier-3 quarantine both assume a stream is
/// *supposed* to keep reporting: they read silence as failure and a
/// sudden step as instability. That assumption holds for a sensor
/// buried in a garden bed and breaks completely for one on a bench,
/// where being unplugged for three weeks between sessions and jumping
/// from air to water between readings is the experiment working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamPolicy {
    /// Tier-1 per-reading rules, Tier-2 health scoring, Tier-3
    /// quarantine. The default for anything not configured otherwise.
    #[default]
    Full,
    /// Tier-1 per-reading rules only. No health state is accumulated
    /// and no quarantine state machine is created, so these streams
    /// never appear in health scores, quarantine listings, or the
    /// gauges derived from them.
    Tier1Only,
}

/// Per-source-group configuration: a policy plus optional metric
/// overrides.
///
/// A *source group* is the part of a source id before the first `/` —
/// `"bed"` in `"bed/1"`, `"charstation"` in `"charstation/3"`. (The
/// GroundTruth server calls this a zone.) Grouping at that level is
/// what lets a whole family of streams share a policy without the
/// group name being spelled out anywhere but the config.
///
/// `metrics` overrides the top-level metric table for this group only.
/// A metric absent here falls back to the global config, so a group
/// that measures `temperature` the same way everyone else does needs
/// no entry at all.
#[derive(Debug, Clone, Default)]
pub struct SourceGroupConfig {
    pub policy: StreamPolicy,
    pub metrics: HashMap<String, MetricConfig>,
}

impl SourceGroupConfig {
    pub fn new(policy: StreamPolicy) -> Self {
        Self {
            policy,
            metrics: HashMap::new(),
        }
    }

    /// Shorthand for [`StreamPolicy::Tier1Only`].
    pub fn tier1_only() -> Self {
        Self::new(StreamPolicy::Tier1Only)
    }

    /// Override one metric's config for this group only.
    pub fn metric(mut self, name: impl Into<String>, config: MetricConfig) -> Self {
        self.metrics.insert(name.into(), config);
        self
    }
}

/// The group a source belongs to: everything before the first `/`, or
/// the whole string if there is no `/`.
pub fn source_group(source: &str) -> &str {
    source.split_once('/').map(|(g, _)| g).unwrap_or(source)
}

/// Top-level validator configuration. Holds per-metric configs plus
/// global Tier-2 / quarantine knobs. Build via [`ValidatorConfig::builder`].
#[derive(Debug, Clone)]
pub struct ValidatorConfig {
    pub metrics: HashMap<String, MetricConfig>,
    /// Per-source-group policies and metric overrides, keyed by group
    /// name (see [`source_group`]). Groups absent here get
    /// [`StreamPolicy::Full`] and the global metric table.
    pub source_groups: HashMap<String, SourceGroupConfig>,
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
            source_groups: HashMap::new(),
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

    /// The config for `source`'s group, if one was registered.
    pub fn group(&self, source: &str) -> Option<&SourceGroupConfig> {
        self.source_groups.get(source_group(source))
    }

    /// Which tiers apply to `source`. Unregistered groups get
    /// [`StreamPolicy::Full`].
    pub fn policy_for(&self, source: &str) -> StreamPolicy {
        self.group(source).map(|g| g.policy).unwrap_or_default()
    }

    /// The effective metric config for one stream: the source group's
    /// override if it has one, else the global entry, else `None`
    /// (which makes Tier-1 a no-op for that metric).
    pub fn metric_for(&self, source: &str, metric: &str) -> Option<&MetricConfig> {
        self.group(source)
            .and_then(|g| g.metrics.get(metric))
            .or_else(|| self.metric(metric))
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

    /// Register a policy (and any metric overrides) for a source group.
    pub fn source_group(mut self, name: impl Into<String>, config: SourceGroupConfig) -> Self {
        self.inner.source_groups.insert(name.into(), config);
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

    #[test]
    fn stuck_mode_defaults_to_count() {
        let m = MetricConfig::new(0.0..=100.0);
        assert_eq!(m.stuck_mode, StuckMode::Count);
        assert!(m.resolution.is_none());
        assert_eq!(m.stuck_window, Duration::minutes(60));
        // With no resolution set, tolerance falls back to the noise floor.
        assert_eq!(m.stuck_tolerance(), 0.01);
    }

    #[test]
    fn time_based_stuck_sets_all_three_parameters() {
        let m = MetricConfig::new(-40.0..=200.0).with_time_based_stuck(0.1, Duration::minutes(90));
        assert_eq!(m.stuck_mode, StuckMode::Duration);
        assert_eq!(m.resolution, Some(0.1));
        assert_eq!(m.stuck_window, Duration::minutes(90));
        assert_eq!(m.stuck_tolerance(), 0.1);
    }

    #[test]
    fn duration_mode_without_resolution_falls_back_to_threshold() {
        let m = MetricConfig::new(0.0..=100.0)
            .with_stuck_mode(StuckMode::Duration)
            .with_stuck_threshold(0.25);
        assert_eq!(m.stuck_tolerance(), 0.25);
    }

    #[test]
    fn stuck_defaults_are_metric_specific() {
        let temp = MetricConfig::new(-40.0..=200.0).with_stuck_defaults_for("temperature");
        let humidity = MetricConfig::new(0.0..=100.0).with_stuck_defaults_for("Humidity");
        let moisture = MetricConfig::new(0.0..=100.0).with_stuck_defaults_for("moisture");
        let raw = MetricConfig::new(0.0..=4095.0).with_stuck_defaults_for("raw_adc");

        for quantized in [&temp, &humidity] {
            assert_eq!(quantized.stuck_mode, StuckMode::Duration);
            assert_eq!(quantized.resolution, Some(0.1));
            assert_eq!(quantized.stuck_window, Duration::minutes(60));
        }
        for analog in [&moisture, &raw] {
            assert_eq!(analog.stuck_mode, StuckMode::Count);
            assert_eq!(analog.stuck_count, 6);
            assert_eq!(analog.stuck_threshold, 0.01);
        }
    }

    #[test]
    fn stuck_defaults_do_not_clobber_other_fields() {
        let m = MetricConfig::new(-40.0..=200.0)
            .with_max_rate_of_change(20.0)
            .with_expected_cadence(Duration::seconds(30))
            .with_stuck_defaults_for("temperature");
        assert_eq!(m.max_rate_of_change, 20.0);
        assert_eq!(m.expected_cadence, Duration::seconds(30));
    }

    #[test]
    fn source_group_splits_on_first_slash() {
        assert_eq!(source_group("bed/1"), "bed");
        assert_eq!(source_group("charstation/12"), "charstation");
        assert_eq!(source_group("greenhouse"), "greenhouse");
        assert_eq!(source_group(""), "");
    }

    #[test]
    fn unregistered_groups_get_the_full_pipeline() {
        let cfg = ValidatorConfig::builder().build();
        assert_eq!(cfg.policy_for("bed/1"), StreamPolicy::Full);
        assert!(cfg.group("bed/1").is_none());
    }

    #[test]
    fn registered_group_policy_applies_to_every_source_in_it() {
        let cfg = ValidatorConfig::builder()
            .source_group("charstation", SourceGroupConfig::tier1_only())
            .build();
        assert_eq!(cfg.policy_for("charstation/1"), StreamPolicy::Tier1Only);
        assert_eq!(cfg.policy_for("charstation/16"), StreamPolicy::Tier1Only);
        assert_eq!(cfg.policy_for("bed/1"), StreamPolicy::Full);
    }

    #[test]
    fn group_metric_override_wins_and_falls_back() {
        let cfg = ValidatorConfig::builder()
            .metric(
                "raw_adc",
                MetricConfig::new(0.0..=4095.0).with_max_rate_of_change(480.0),
            )
            .metric("temperature", MetricConfig::new(-40.0..=200.0))
            .source_group(
                "charstation",
                SourceGroupConfig::tier1_only().metric(
                    "raw_adc",
                    MetricConfig::new(0.0..=4095.0).without_rate_check(),
                ),
            )
            .build();

        // Overridden for the group...
        assert!(cfg
            .metric_for("charstation/3", "raw_adc")
            .unwrap()
            .max_rate_of_change
            .is_infinite());
        // ...untouched for everyone else...
        assert_eq!(
            cfg.metric_for("bed/1", "raw_adc")
                .unwrap()
                .max_rate_of_change,
            480.0
        );
        // ...and metrics the group doesn't override fall back to global.
        assert_eq!(
            cfg.metric_for("charstation/3", "temperature")
                .unwrap()
                .valid_range,
            -40.0..=200.0
        );
        // Unknown metrics stay unknown.
        assert!(cfg.metric_for("charstation/3", "pressure").is_none());
    }

    #[test]
    fn without_rate_check_disables_the_rule() {
        let m = MetricConfig::new(0.0..=4095.0)
            .with_max_rate_of_change(480.0)
            .without_rate_check();
        assert!(m.max_rate_of_change.is_infinite());
    }

    #[test]
    fn history_cap_never_zero() {
        assert_eq!(MetricConfig::new(0.0..=1.0).history_cap(), 5);
        assert_eq!(
            MetricConfig::new(0.0..=1.0)
                .with_stuck_count(1)
                .history_cap(),
            1
        );
        assert_eq!(
            MetricConfig::new(0.0..=1.0)
                .with_stuck_count(0)
                .history_cap(),
            1
        );
    }
}
