//! Tier-1 validation: per-reading rules. Each rule inspects a single
//! incoming [`Reading`] (optionally with help from the source's recent
//! history) and may emit a non-`Good` [`ValidationResult`].
//!
//! Rules return [`Option<ValidationResult>`]:
//! - `None` means the rule passed silently.
//! - `Some(r)` means the rule fired and `r` carries the severity + reason.
//!
//! [`run_tier1`] orchestrates the four rules in priority order with
//! short-circuiting on `Invalid` (no point checking history when the
//! value itself is impossible).

use crate::config::MetricConfig;
use crate::reading::{QualityLevel, Reading, ValidationResult};
use chrono::{DateTime, Utc};
use std::collections::VecDeque;

/// Per-source-and-metric history maintained between calls to the
/// validator. Holds the most recent readings (regardless of quality)
/// up to a cap derived from `MetricConfig::stuck_count`.
#[derive(Debug, Clone, Default)]
pub struct SourceState {
    pub history: VecDeque<HistoricalReading>,
}

#[derive(Debug, Clone, Copy)]
pub struct HistoricalReading {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
}

impl SourceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a reading to history, evicting the oldest if `cap` is
    /// exceeded. `cap` is typically `stuck_count - 1` (we never need
    /// more history than that for Tier-1).
    pub fn push(&mut self, timestamp: DateTime<Utc>, value: f64, cap: usize) {
        let cap = cap.max(1);
        self.history.push_back(HistoricalReading { timestamp, value });
        while self.history.len() > cap {
            self.history.pop_front();
        }
    }

    pub fn most_recent(&self) -> Option<HistoricalReading> {
        self.history.back().copied()
    }
}

/// Rule 1: value range. NaN or out-of-range → Invalid.
pub fn validate_value_range(
    reading: &Reading,
    config: &MetricConfig,
) -> Option<ValidationResult> {
    if reading.value.is_nan() {
        return Some(ValidationResult::invalid(
            "value_range",
            format!("{} value is NaN", reading.metric),
        ));
    }
    if !config.valid_range.contains(&reading.value) {
        return Some(ValidationResult::invalid(
            "value_range",
            format!(
                "{} = {:.2} outside plausible range [{}, {}]",
                reading.metric,
                reading.value,
                config.valid_range.start(),
                config.valid_range.end(),
            ),
        ));
    }
    None
}

/// Rule 2: raw transducer range. Only runs if the reading carries a
/// raw value and the config specifies a raw range.
pub fn validate_raw_range(
    reading: &Reading,
    config: &MetricConfig,
) -> Option<ValidationResult> {
    let raw = reading.raw_value?;
    let range = config.raw_valid_range.as_ref()?;
    if !range.contains(&raw) {
        return Some(ValidationResult::invalid(
            "raw_range",
            format!(
                "raw value = {} outside plausible range [{}, {}]",
                raw,
                range.start(),
                range.end(),
            ),
        ));
    }
    None
}

/// Rule 3: stuck reading. If the previous `stuck_count - 1` readings
/// and the new reading are all within `stuck_threshold` of each other,
/// the sensor is probably stuck. Insufficient history → no result.
pub fn check_stuck(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> Option<ValidationResult> {
    let required_prior = config.stuck_count.saturating_sub(1);
    if state.history.len() < required_prior {
        return None;
    }
    let recent = state.history.iter().rev().take(required_prior);
    let all_match = recent.into_iter().all(|h| {
        (h.value - reading.value).abs() <= config.stuck_threshold
    });
    if all_match {
        Some(ValidationResult::suspect(
            "stuck_reading",
            format!(
                "{} consecutive readings within {} of {:.2} — source may be stuck",
                config.stuck_count, config.stuck_threshold, reading.value
            ),
        ))
    } else {
        None
    }
}

/// Rule 4: rate of change. If the previous reading is within
/// `rate_window` and the absolute delta exceeds `max_rate_of_change`,
/// flag as Suspect.
pub fn check_rate_of_change(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> Option<ValidationResult> {
    if !config.max_rate_of_change.is_finite() {
        return None;
    }
    let prev = state.most_recent()?;
    let dt = reading.timestamp - prev.timestamp;
    if dt.num_seconds() <= 0 || dt > config.rate_window {
        return None;
    }
    let delta = (reading.value - prev.value).abs();
    if delta > config.max_rate_of_change {
        Some(ValidationResult::suspect(
            "rate_of_change",
            format!(
                "{} changed by {:.2} in {}s (threshold {:.2}/{}s)",
                reading.metric,
                delta,
                dt.num_seconds(),
                config.max_rate_of_change,
                config.rate_window.num_seconds(),
            ),
        ))
    } else {
        None
    }
}

/// Run all four Tier-1 rules in priority order, short-circuiting once
/// an Invalid verdict is reached. Returns the merged result.
pub fn run_tier1(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> ValidationResult {
    let mut result = ValidationResult::good();

    if let Some(r) = validate_value_range(reading, config) {
        result = result.merge(r);
        if result.quality == QualityLevel::Invalid {
            return result;
        }
    }

    if let Some(r) = validate_raw_range(reading, config) {
        result = result.merge(r);
        if result.quality == QualityLevel::Invalid {
            return result;
        }
    }

    if let Some(r) = check_stuck(reading, config, state) {
        result = result.merge(r);
    }
    if let Some(r) = check_rate_of_change(reading, config, state) {
        result = result.merge(r);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn moisture_config() -> MetricConfig {
        MetricConfig::new(0.0..=100.0)
            .with_raw_range(100..=3995)
            .with_max_rate_of_change(30.0)
            .with_rate_window(Duration::seconds(600))
    }

    fn temp_config() -> MetricConfig {
        MetricConfig::new(-40.0..=200.0).with_max_rate_of_change(20.0)
    }

    fn r(value: f64, secs_ago: i64) -> Reading {
        Reading::new(
            "bed/1",
            "moisture",
            value,
            Utc::now() - Duration::seconds(secs_ago),
        )
    }

    fn r_with_raw(value: f64, raw: i64) -> Reading {
        r(value, 0).with_raw(raw)
    }

    #[test]
    fn moisture_in_range_is_good() {
        let cfg = moisture_config();
        let res = run_tier1(&r(42.5, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn moisture_above_range_is_invalid() {
        let cfg = moisture_config();
        let res = run_tier1(&r(105.0, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
        assert_eq!(res.rule, "value_range");
    }

    #[test]
    fn negative_moisture_is_invalid() {
        let cfg = moisture_config();
        let res = run_tier1(&r(-5.0, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
    }

    #[test]
    fn temperature_extreme_cold_is_invalid() {
        let cfg = temp_config();
        let reading = Reading::new("h/1", "temperature", -100.0, Utc::now());
        let res = run_tier1(&reading, &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
    }

    #[test]
    fn nan_is_invalid() {
        let cfg = moisture_config();
        let res = run_tier1(&r(f64::NAN, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
    }

    #[test]
    fn raw_in_range_passes() {
        let cfg = moisture_config();
        let res = run_tier1(&r_with_raw(42.0, 2400), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn raw_zero_is_invalid() {
        let cfg = moisture_config();
        let res = run_tier1(&r_with_raw(42.0, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
        assert_eq!(res.rule, "raw_range");
    }

    #[test]
    fn raw_at_max_is_invalid() {
        let cfg = moisture_config();
        let res = run_tier1(&r_with_raw(42.0, 4095), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
    }

    #[test]
    fn invalid_value_short_circuits_raw_check() {
        // Out-of-range value should produce value_range failure, not
        // raw_range, even though raw is also bad.
        let cfg = moisture_config();
        let res = run_tier1(&r_with_raw(200.0, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Invalid);
        assert_eq!(res.rule, "value_range");
    }

    #[test]
    fn stuck_six_identical_readings_is_suspect() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        for i in 0..5 {
            state.push(
                Utc::now() - Duration::seconds(60 * (5 - i)),
                42.5,
                cfg.stuck_count - 1,
            );
        }
        let res = run_tier1(&r(42.5, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Suspect);
        assert_eq!(res.rule, "stuck_reading");
    }

    #[test]
    fn stuck_with_variation_is_good() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        let vs = [42.5, 42.5, 42.5, 43.0, 42.5];
        for (i, v) in vs.iter().enumerate() {
            state.push(
                Utc::now() - Duration::seconds(60 * (5 - i as i64)),
                *v,
                cfg.stuck_count - 1,
            );
        }
        let res = run_tier1(&r(42.5, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn stuck_insufficient_history_is_good() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(Utc::now() - Duration::seconds(60), 42.5, cfg.stuck_count - 1);
        let res = run_tier1(&r(42.5, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn rapid_change_is_suspect() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(Utc::now() - Duration::seconds(60), 20.0, cfg.stuck_count - 1);
        let res = run_tier1(&r(80.0, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Suspect);
        assert_eq!(res.rule, "rate_of_change");
    }

    #[test]
    fn small_change_is_good() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(Utc::now() - Duration::seconds(60), 40.0, cfg.stuck_count - 1);
        let res = run_tier1(&r(42.0, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn change_outside_window_is_good() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(
            Utc::now() - Duration::seconds(3600),
            20.0,
            cfg.stuck_count - 1,
        );
        let res = run_tier1(&r(80.0, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn no_prior_reading_skips_rate_check() {
        let cfg = moisture_config();
        let res = run_tier1(&r(80.0, 0), &cfg, &SourceState::new());
        assert_eq!(res.quality, QualityLevel::Good);
    }
}
