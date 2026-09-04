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

use crate::config::{MetricConfig, StuckMode};
use crate::reading::{QualityLevel, Reading, ValidationResult};
use chrono::{DateTime, Utc};
use std::collections::VecDeque;

/// Per-source-and-metric history maintained between calls to the
/// validator. Holds the most recent readings (regardless of quality)
/// up to a cap derived from `MetricConfig::stuck_count`, plus the
/// anchor of the current unchanged run for time-based stuck detection.
#[derive(Debug, Clone, Default)]
pub struct SourceState {
    pub history: VecDeque<HistoricalReading>,
    /// Oldest reading of the run of readings that have all stayed
    /// within tolerance of it. Maintained by
    /// [`SourceState::push_with_config`]; `None` until the first such
    /// push. Tracking the run's start point keeps time-based stuck
    /// detection O(1) and independent of reporting cadence — buffering
    /// a whole `stuck_window` of readings would not be.
    stuck_anchor: Option<HistoricalReading>,
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
    ///
    /// This maintains the bounded history only. Callers using
    /// [`StuckMode::Duration`] want [`Self::push_with_config`], which
    /// also advances the unchanged-run anchor.
    pub fn push(&mut self, timestamp: DateTime<Utc>, value: f64, cap: usize) {
        let cap = cap.max(1);
        self.history
            .push_back(HistoricalReading { timestamp, value });
        while self.history.len() > cap {
            self.history.pop_front();
        }
    }

    /// Append a reading, sizing the history from `config` and updating
    /// the unchanged-run anchor: the run continues while the new value
    /// stays within `config.stuck_tolerance()` of the anchor's value,
    /// and restarts at this reading as soon as it doesn't.
    pub fn push_with_config(
        &mut self,
        timestamp: DateTime<Utc>,
        value: f64,
        config: &MetricConfig,
    ) {
        let reading = HistoricalReading { timestamp, value };
        let continues_run = self
            .stuck_anchor
            .map(|a| (value - a.value).abs() <= config.stuck_tolerance())
            .unwrap_or(false);
        if !continues_run {
            self.stuck_anchor = Some(reading);
        }
        self.push(timestamp, value, config.history_cap());
    }

    pub fn most_recent(&self) -> Option<HistoricalReading> {
        self.history.back().copied()
    }

    /// Start of the current unchanged run, if one has been established.
    pub fn stuck_anchor(&self) -> Option<HistoricalReading> {
        self.stuck_anchor
    }
}

/// Rule 1: value range. NaN or out-of-range → Invalid.
pub fn validate_value_range(reading: &Reading, config: &MetricConfig) -> Option<ValidationResult> {
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
pub fn validate_raw_range(reading: &Reading, config: &MetricConfig) -> Option<ValidationResult> {
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

/// Rule 3: stuck reading. Dispatches on [`StuckMode`] — see that type
/// for why the right question differs by metric.
pub fn check_stuck(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> Option<ValidationResult> {
    match config.stuck_mode {
        StuckMode::Count => check_stuck_count(reading, config, state),
        StuckMode::Duration => check_stuck_duration(reading, config, state),
    }
}

/// [`StuckMode::Count`]: if the previous `stuck_count - 1` readings and
/// the new reading are all within `stuck_threshold` of each other, the
/// sensor is probably stuck. Insufficient history → no result.
pub fn check_stuck_count(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> Option<ValidationResult> {
    let required_prior = config.stuck_count.saturating_sub(1);
    if state.history.len() < required_prior {
        return None;
    }
    let recent = state.history.iter().rev().take(required_prior);
    let all_match = recent
        .into_iter()
        .all(|h| (h.value - reading.value).abs() <= config.stuck_threshold);
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

/// [`StuckMode::Duration`]: if the value has not moved more than
/// `resolution` away from where it was `stuck_window` ago, the source
/// is probably stuck. Cadence is irrelevant here — a 10-second and a
/// 10-minute publisher both need the same wall-clock stillness before
/// they are flagged, which is exactly what count-based detection gets
/// wrong for quantized metrics.
///
/// No anchor (the source has never been pushed through
/// [`SourceState::push_with_config`]) → no result.
pub fn check_stuck_duration(
    reading: &Reading,
    config: &MetricConfig,
    state: &SourceState,
) -> Option<ValidationResult> {
    let tolerance = config.stuck_tolerance();
    let anchor = state.stuck_anchor()?;
    if (reading.value - anchor.value).abs() > tolerance {
        return None;
    }
    let unchanged_for = reading.timestamp - anchor.timestamp;
    if unchanged_for < config.stuck_window {
        return None;
    }
    Some(ValidationResult::suspect(
        "stuck_reading",
        format!(
            "value unchanged ({:.2} ±{}) for {} min — source may be stuck",
            reading.value,
            tolerance,
            unchanged_for.num_minutes(),
        ),
    ))
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
        state.push(
            Utc::now() - Duration::seconds(60),
            42.5,
            cfg.stuck_count - 1,
        );
        let res = run_tier1(&r(42.5, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Good);
    }

    #[test]
    fn rapid_change_is_suspect() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(
            Utc::now() - Duration::seconds(60),
            20.0,
            cfg.stuck_count - 1,
        );
        let res = run_tier1(&r(80.0, 0), &cfg, &state);
        assert_eq!(res.quality, QualityLevel::Suspect);
        assert_eq!(res.rule, "rate_of_change");
    }

    #[test]
    fn small_change_is_good() {
        let cfg = moisture_config();
        let mut state = SourceState::new();
        state.push(
            Utc::now() - Duration::seconds(60),
            40.0,
            cfg.stuck_count - 1,
        );
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

    // ---- Time-based stuck detection -------------------------------
    //
    // The scenario throughout: a DHT22 in a garage. 0.1 °F resolution,
    // ~1 °F/hour of real change, published every 10 seconds. Under the
    // count-based rule that stream is ~always "6 consecutive readings
    // within 0.01" — which is the sensor working, not failing.

    const CADENCE_SECS: i64 = 10;

    fn dht22_temp_config() -> MetricConfig {
        MetricConfig::new(-40.0..=200.0)
            .with_max_rate_of_change(20.0)
            .with_expected_cadence(Duration::seconds(CADENCE_SECS))
            .with_stuck_defaults_for("temperature")
    }

    /// Feed a series of `(offset_seconds, value)` samples through
    /// Tier-1 in order, returning every non-Good result. Mirrors what
    /// `StreamValidator::validate` does: check, then record.
    fn replay(
        metric: &str,
        cfg: &MetricConfig,
        samples: impl IntoIterator<Item = (i64, f64)>,
    ) -> Vec<(i64, ValidationResult)> {
        let start = Utc::now();
        let mut state = SourceState::new();
        let mut flagged = Vec::new();
        for (offset, value) in samples {
            let reading = Reading::new("bed/1", metric, value, start + Duration::seconds(offset));
            let res = run_tier1(&reading, cfg, &state);
            if res.quality != QualityLevel::Good {
                flagged.push((offset, res));
            }
            state.push_with_config(reading.timestamp, reading.value, cfg);
        }
        flagged
    }

    /// A DHT22 warming up: +0.1 °F every 5 minutes (~1.2 °F/hour),
    /// sampled every 10 seconds, for three hours.
    fn warmup_ramp() -> Vec<(i64, f64)> {
        let samples = 3 * 3600 / CADENCE_SECS;
        (0..samples)
            .map(|i| {
                let t = i * CADENCE_SECS;
                (t, 65.8 + (t / 300) as f64 * 0.1)
            })
            .collect()
    }

    #[test]
    fn slow_ramp_as_temperature_is_never_stuck() {
        let flagged = replay("temperature", &dht22_temp_config(), warmup_ramp());
        assert!(
            flagged.is_empty(),
            "a working DHT22 should produce no flags, got {} (first: {:?})",
            flagged.len(),
            flagged.first(),
        );
    }

    #[test]
    fn slow_ramp_as_moisture_is_stuck_by_the_count_rule() {
        // Same data, count-based rule: this documents that the rule is
        // metric-specific on purpose. A soil probe that repeated a
        // value 30 times running really would be broken.
        let flagged = replay("moisture", &moisture_config(), warmup_ramp());
        assert!(
            !flagged.is_empty(),
            "count-based rule should flag the identical-value runs"
        );
        assert!(flagged.iter().all(|(_, r)| r.rule == "stuck_reading"));
    }

    #[test]
    fn frozen_temperature_is_flagged_only_after_the_window() {
        // Truly stuck: the same 68.70 for two hours at 10s cadence.
        let samples = 2 * 3600 / CADENCE_SECS;
        let frozen = (0..samples).map(|i| (i * CADENCE_SECS, 68.70));
        let flagged = replay("temperature", &dht22_temp_config(), frozen);

        let first = flagged.first().expect("a frozen sensor must be flagged");
        assert_eq!(first.1.rule, "stuck_reading");
        assert_eq!(first.1.quality, QualityLevel::Suspect);
        assert!(
            first.0 >= 3600,
            "first flag came at {}s, before the 60-minute window",
            first.0
        );
        assert_eq!(first.0, 3600, "and should come exactly at the window");
        assert_eq!(
            first.1.reason,
            "value unchanged (68.70 ±0.1) for 60 min — source may be stuck"
        );

        // It stays flagged, and the reported duration keeps growing.
        let last = flagged.last().unwrap();
        assert!(last.1.reason.contains("for 119 min"), "{}", last.1.reason);
    }

    #[test]
    fn frozen_value_that_resumes_before_the_window_is_never_flagged() {
        // Silent-ish for 45 minutes, then the real world moves it again.
        let samples = 2 * 3600 / CADENCE_SECS;
        let resuming = (0..samples).map(|i| {
            let t = i * CADENCE_SECS;
            let value = if t < 45 * 60 {
                68.70
            } else {
                // Back to moving: +0.1 °F per minute from 45 min on,
                // which clears the 0.1 resolution within one minute.
                68.70 + ((t - 45 * 60) / 60 + 1) as f64 * 0.1
            };
            (t, value)
        });
        let flagged = replay("temperature", &dht22_temp_config(), resuming);
        assert!(
            flagged.is_empty(),
            "45 min < the 60 min window, so nothing should fire: {:?}",
            flagged.first()
        );
    }

    #[test]
    fn duration_mode_needs_an_anchor() {
        // A bare SourceState has no unchanged run, so the very first
        // reading of a stream can never be stuck.
        let cfg = dht22_temp_config();
        let reading = Reading::new("bed/1", "temperature", 68.7, Utc::now());
        assert!(check_stuck(&reading, &cfg, &SourceState::new()).is_none());
    }

    #[test]
    fn duration_mode_ignores_out_of_order_timestamps() {
        let cfg = dht22_temp_config();
        let now = Utc::now();
        let mut state = SourceState::new();
        state.push_with_config(now, 68.7, &cfg);
        // A reading stamped two hours *before* the anchor must not read
        // as "unchanged for -120 min".
        let stale = Reading::new("bed/1", "temperature", 68.7, now - Duration::hours(2));
        assert!(check_stuck(&stale, &cfg, &state).is_none());
    }

    #[test]
    fn duration_mode_resets_the_anchor_when_the_value_moves() {
        let cfg = dht22_temp_config();
        let now = Utc::now();
        let mut state = SourceState::new();
        state.push_with_config(now, 68.7, &cfg);
        assert_eq!(state.stuck_anchor().unwrap().value, 68.7);
        // Within resolution → same run, anchor unmoved.
        state.push_with_config(now + Duration::minutes(30), 68.8, &cfg);
        assert_eq!(state.stuck_anchor().unwrap().timestamp, now);
        // Beyond resolution → new run starts here.
        let moved_at = now + Duration::minutes(40);
        state.push_with_config(moved_at, 69.0, &cfg);
        assert_eq!(state.stuck_anchor().unwrap().timestamp, moved_at);
    }
}
