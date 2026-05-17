//! [`StreamValidator`] — the high-level entry point. Wraps the Tier-1,
//! Tier-2, and quarantine layers behind a small mutable-state API.
//!
//! Usage:
//! ```no_run
//! use groundtruth_validator::{
//!     MetricConfig, Reading, StreamValidator, ValidatorConfig,
//! };
//! use chrono::Utc;
//!
//! let config = ValidatorConfig::builder()
//!     .metric("moisture", MetricConfig::new(0.0..=100.0))
//!     .build();
//! let mut validator = StreamValidator::new(config);
//!
//! let result = validator.validate(Reading::new("bed/1", "moisture", 42.5, Utc::now()));
//! assert_eq!(result.quality.as_str(), "good");
//!
//! // Call periodically (e.g. every 30s) to refresh Tier-2 scores.
//! let _scores = validator.update_health();
//! let _transitions = validator.update_quarantine();
//! ```

use crate::config::ValidatorConfig;
use crate::quarantine::{update_quarantine, QuarantineState, QuarantineTransition};
use crate::reading::{Reading, ValidationResult};
use crate::tier1::{run_tier1, SourceState};
use crate::tier2::{compute_health_score, HealthScore, HealthState};
use chrono::Utc;
use std::collections::HashMap;

/// Key for per-source data: `(source, metric)`. A source can publish
/// multiple metrics and each gets independent state.
pub type SourceMetric = (String, String);

pub struct StreamValidator {
    config: ValidatorConfig,
    source_state: HashMap<SourceMetric, SourceState>,
    health_state: HashMap<SourceMetric, HealthState>,
    quarantine_state: HashMap<SourceMetric, QuarantineState>,
    /// Cached output of the last `update_health` call, indexed by
    /// `(source, metric)`. Used by `quarantine_status` and the
    /// quarantine update loop.
    last_health_scores: HashMap<SourceMetric, HealthScore>,
}

impl StreamValidator {
    pub fn new(config: ValidatorConfig) -> Self {
        Self {
            config,
            source_state: HashMap::new(),
            health_state: HashMap::new(),
            quarantine_state: HashMap::new(),
            last_health_scores: HashMap::new(),
        }
    }

    pub fn config(&self) -> &ValidatorConfig {
        &self.config
    }

    /// Validate one reading and update the internal state. Tier-1
    /// rules run against the source's recent history; the reading is
    /// then appended to that history, and the reading's quality flag
    /// is recorded in the Tier-2 health state.
    ///
    /// If no `MetricConfig` exists for `reading.metric`, Tier-1 is
    /// skipped (result = `Good`) and the reading is still tracked for
    /// Tier-2 — a minimal no-op default rather than a hard error, so
    /// callers can ingest first and configure later.
    pub fn validate(&mut self, reading: Reading) -> ValidationResult {
        let key: SourceMetric = (reading.source.clone(), reading.metric.clone());

        let result = match self.config.metric(&reading.metric) {
            Some(metric_config) => {
                let state = self.source_state.entry(key.clone()).or_default();
                let result = run_tier1(&reading, metric_config, state);
                state.push(
                    reading.timestamp,
                    reading.value,
                    metric_config.stuck_count.saturating_sub(1).max(1),
                );
                result
            }
            None => ValidationResult::good(),
        };

        let health_state = self.health_state.entry(key).or_default();
        health_state.push(
            reading.timestamp,
            reading.value,
            result.quality,
            self.config.baseline_window,
        );

        result
    }

    /// Recompute health scores for every tracked source. Returns
    /// (source, metric, score) for each. Also refreshes the internal
    /// cache that quarantine queries consult.
    pub fn update_health(&mut self) -> Vec<(String, String, HealthScore)> {
        let now = Utc::now();
        let mut out = Vec::with_capacity(self.health_state.len());
        let mut cache = HashMap::with_capacity(self.health_state.len());

        for (key, state) in self.health_state.iter() {
            let metric_config = self.config.metric(&key.1);
            let score = compute_health_score(state, &self.config, metric_config, now);
            cache.insert(key.clone(), score.clone());
            out.push((key.0.clone(), key.1.clone(), score));
        }

        self.last_health_scores = cache;
        out
    }

    pub fn health_score(&self, source: &str, metric: &str) -> Option<HealthScore> {
        self.last_health_scores
            .get(&(source.to_string(), metric.to_string()))
            .cloned()
    }

    /// Step each source's quarantine state machine using the most
    /// recent health score. Returns the (source, metric, transition)
    /// triples for every source that was advanced. Call after
    /// [`Self::update_health`] for fresh scores.
    pub fn update_quarantine(&mut self) -> Vec<(String, String, QuarantineTransition)> {
        let now = Utc::now();
        let mut out = Vec::with_capacity(self.last_health_scores.len());

        for (key, score) in self.last_health_scores.iter() {
            let state = self.quarantine_state.entry(key.clone()).or_default();
            let transition = update_quarantine(state, score.overall, &self.config, now);
            out.push((key.0.clone(), key.1.clone(), transition));
        }

        out
    }

    pub fn quarantine_status(&self, source: &str, metric: &str) -> Option<&QuarantineState> {
        self.quarantine_state
            .get(&(source.to_string(), metric.to_string()))
    }

    pub fn is_quarantined(&self, source: &str, metric: &str) -> bool {
        self.quarantine_status(source, metric)
            .map(|s| s.is_quarantined)
            .unwrap_or(false)
    }

    /// All `(source, metric)` pairs the validator has seen, sorted for
    /// stable iteration.
    pub fn sources(&self) -> Vec<SourceMetric> {
        let mut keys: Vec<_> = self.health_state.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Iterator over the most recent health scores. Empty until
    /// `update_health` has run at least once.
    pub fn health_scores(&self) -> impl Iterator<Item = (&SourceMetric, &HealthScore)> {
        self.last_health_scores.iter()
    }

    /// Iterator over current quarantine states (only those that have
    /// been touched by an `update_quarantine` call).
    pub fn quarantine_states(&self) -> impl Iterator<Item = (&SourceMetric, &QuarantineState)> {
        self.quarantine_state.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MetricConfig;
    use crate::reading::QualityLevel;
    use chrono::{Duration, Utc};

    fn cfg() -> ValidatorConfig {
        ValidatorConfig::builder()
            .metric(
                "moisture",
                MetricConfig::new(0.0..=100.0)
                    .with_raw_range(100..=3995)
                    .with_max_rate_of_change(30.0)
                    .with_rate_window(Duration::seconds(600))
                    .with_expected_cadence(Duration::seconds(60)),
            )
            .metric(
                "temperature",
                MetricConfig::new(-40.0..=200.0).with_max_rate_of_change(20.0),
            )
            .build()
    }

    #[test]
    fn validate_good_reading() {
        let mut v = StreamValidator::new(cfg());
        let r = Reading::new("bed/1", "moisture", 42.0, Utc::now());
        let res = v.validate(r);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn validate_out_of_range() {
        let mut v = StreamValidator::new(cfg());
        let r = Reading::new("bed/1", "moisture", 150.0, Utc::now());
        let res = v.validate(r);
        assert_eq!(res.quality, QualityLevel::Invalid);
    }

    #[test]
    fn sequence_detects_stuck_reading() {
        let mut v = StreamValidator::new(cfg());
        let base = Utc::now() - Duration::seconds(60 * 10);
        // Six consecutive identical readings — the sixth should fire.
        for i in 0..6 {
            let r = Reading::new(
                "bed/1",
                "moisture",
                42.5,
                base + Duration::seconds(60 * i),
            );
            let res = v.validate(r);
            if i < 5 {
                assert_eq!(res.quality, QualityLevel::Good, "i={i}");
            } else {
                assert_eq!(res.quality, QualityLevel::Suspect);
                assert_eq!(res.rule, "stuck_reading");
            }
        }
    }

    #[test]
    fn sequence_detects_rapid_change() {
        let mut v = StreamValidator::new(cfg());
        let now = Utc::now();
        v.validate(Reading::new(
            "bed/1",
            "moisture",
            20.0,
            now - Duration::seconds(60),
        ));
        let res = v.validate(Reading::new("bed/1", "moisture", 80.0, now));
        assert_eq!(res.quality, QualityLevel::Suspect);
        assert_eq!(res.rule, "rate_of_change");
    }

    #[test]
    fn unknown_metric_passes_through_as_good() {
        let mut v = StreamValidator::new(cfg());
        let r = Reading::new("bed/1", "vibration", 9999.9, Utc::now());
        let res = v.validate(r);
        assert_eq!(res.quality, QualityLevel::Good);
        // It should still be tracked for Tier-2.
        assert!(v
            .sources()
            .contains(&("bed/1".to_string(), "vibration".to_string())));
    }

    #[test]
    fn multiple_sources_tracked_independently() {
        let mut v = StreamValidator::new(cfg());
        let now = Utc::now();
        // Source A has a stuck pattern; source B has varied readings.
        // The stuck rule on A must not affect B.
        for i in 0..6 {
            v.validate(Reading::new(
                "bed/A",
                "moisture",
                42.5,
                now + Duration::seconds(60 * i),
            ));
            v.validate(Reading::new(
                "bed/B",
                "moisture",
                40.0 + i as f64,
                now + Duration::seconds(60 * i),
            ));
        }
        let res_a = v.validate(Reading::new(
            "bed/A",
            "moisture",
            42.5,
            now + Duration::seconds(60 * 6),
        ));
        let res_b = v.validate(Reading::new(
            "bed/B",
            "moisture",
            45.0,
            now + Duration::seconds(60 * 6),
        ));
        assert_eq!(res_a.quality, QualityLevel::Suspect);
        assert_eq!(res_b.quality, QualityLevel::Good);
    }

    #[test]
    fn update_health_returns_score_per_source() {
        let mut v = StreamValidator::new(cfg());
        let now = Utc::now();
        // Vary slightly so the stuck-reading rule (which fires on six
        // values within 0.01) doesn't trip, but stay well under the
        // 30/600s rate-of-change ceiling. Readings end at `now`.
        for i in 0..30i64 {
            v.validate(Reading::new(
                "bed/1",
                "moisture",
                42.0 + (i as f64) * 0.1,
                now - Duration::seconds(60 * (29 - i)),
            ));
        }
        let scores = v.update_health();
        assert_eq!(scores.len(), 1);
        let (src, metric, score) = &scores[0];
        assert_eq!(src, "bed/1");
        assert_eq!(metric, "moisture");
        assert!(score.recency > 0.9, "recency was {}", score.recency);
        assert!(score.quality_rate > 0.99);

        let cached = v.health_score("bed/1", "moisture").unwrap();
        assert_eq!(cached.overall, score.overall);
    }

    #[test]
    fn quarantine_round_trip_via_update() {
        // Feed bad readings, drive scores low, then call update_quarantine.
        let mut v = StreamValidator::new(cfg());
        // We directly populate health_state with a low-score scenario
        // by inserting suspect/invalid readings.
        let now = Utc::now();
        for i in 0..30 {
            // Suspect readings (out-of-range raw would be invalid; just
            // mark them as poor by ingesting NaN-equivalent — easier
            // to drive low score by skipping ingestion and forcing
            // empty state, which scores 0.
            v.validate(Reading::new(
                "bed/1",
                "moisture",
                42.0,
                now - Duration::hours(2) - Duration::seconds(i),
            ));
        }
        // All readings are outside the recent window → score will be
        // very low because quality_rate=0, recency=0.
        let _scores = v.update_health();
        let mut entered = false;
        for _ in 0..3 {
            let transitions = v.update_quarantine();
            if transitions
                .iter()
                .any(|(_, _, t)| *t == QuarantineTransition::Entered)
            {
                entered = true;
                break;
            }
        }
        assert!(entered, "sensor should have entered quarantine");
        assert!(v.is_quarantined("bed/1", "moisture"));
    }

    #[test]
    fn sources_returns_sorted_list() {
        let mut v = StreamValidator::new(cfg());
        let now = Utc::now();
        v.validate(Reading::new("bed/2", "moisture", 50.0, now));
        v.validate(Reading::new("bed/1", "temperature", 70.0, now));
        v.validate(Reading::new("bed/1", "moisture", 50.0, now));
        let s = v.sources();
        assert_eq!(
            s,
            vec![
                ("bed/1".into(), "moisture".into()),
                ("bed/1".into(), "temperature".into()),
                ("bed/2".into(), "moisture".into()),
            ]
        );
    }
}
