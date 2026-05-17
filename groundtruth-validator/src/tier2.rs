//! Tier-2 validation: per-source health scoring.
//!
//! Tier-1 judges individual readings in isolation. Tier-2 judges
//! sources *over time* by combining five normalized signals into a
//! 0-100 composite score:
//!
//! | Signal             | Weight |
//! |--------------------|--------|
//! | Quality rate       | 35%    |
//! | Reporting cadence  | 25%    |
//! | Baseline drift     | 20%    |
//! | Variance ratio     | 10%    |
//! | Recency            | 10%    |
//!
//! Baselines are computed from `Good` readings only — excluding bad
//! readings from the baseline prevents source degradation from
//! infecting the model used to detect that same degradation.

use crate::config::{MetricConfig, ValidatorConfig};
use crate::reading::QualityLevel;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const QUALITY_WEIGHT: f64 = 0.35;
pub const CADENCE_WEIGHT: f64 = 0.25;
pub const DRIFT_WEIGHT: f64 = 0.20;
pub const VARIANCE_WEIGHT: f64 = 0.10;
pub const RECENCY_WEIGHT: f64 = 0.10;

/// Window over which "recent" signals (quality rate, cadence) are
/// computed. Hardcoded to match the project's reference implementation;
/// expose as config if you need a different horizon.
pub const RECENT_WINDOW: Duration = Duration::minutes(60);

/// A reading is considered "stale" once it's this far past `now` for
/// the recency signal to fully decay to zero.
pub const RECENCY_HORIZON: Duration = Duration::seconds(600);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RecentReading {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
    pub quality: QualityLevel,
}

/// Rolling history for one (source, metric). Bounded by the configured
/// `baseline_window` — readings older than that are evicted on insert.
#[derive(Debug, Clone, Default)]
pub struct HealthState {
    pub history: VecDeque<RecentReading>,
}

impl HealthState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a reading and prune anything outside `baseline_window`.
    pub fn push(
        &mut self,
        timestamp: DateTime<Utc>,
        value: f64,
        quality: QualityLevel,
        baseline_window: Duration,
    ) {
        self.history.push_back(RecentReading {
            timestamp,
            value,
            quality,
        });
        let cutoff = timestamp - baseline_window;
        while let Some(front) = self.history.front() {
            if front.timestamp < cutoff {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }
}

/// The five component signals plus the composite. Component scores are
/// 0.0–1.0 normalized; `overall` is 0.0–100.0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthScore {
    pub overall: f64,
    pub quality_rate: f64,
    pub reporting_cadence: f64,
    pub drift: f64,
    pub variance_ratio: f64,
    pub recency: f64,

    pub readings_in_window: u64,
    pub last_good_reading_at: Option<DateTime<Utc>>,
    pub computed_at: DateTime<Utc>,
}

/// Compute the full health score for a source.
///
/// `metric_config` provides the expected cadence; if absent, cadence
/// defaults to a permissive value (one reading per minute).
pub fn compute_health_score(
    state: &HealthState,
    config: &ValidatorConfig,
    metric_config: Option<&MetricConfig>,
    now: DateTime<Utc>,
) -> HealthScore {
    let recent_cutoff = now - RECENT_WINDOW;
    let recent: Vec<&RecentReading> = state
        .history
        .iter()
        .filter(|r| r.timestamp >= recent_cutoff)
        .collect();

    // --- Signal 1: quality rate ---
    let quality_rate = if recent.is_empty() {
        0.0
    } else {
        let good = recent
            .iter()
            .filter(|r| r.quality == QualityLevel::Good)
            .count();
        good as f64 / recent.len() as f64
    };

    // --- Signal 2: reporting cadence ---
    let expected_interval = metric_config
        .map(|m| m.expected_cadence.num_seconds().max(1))
        .unwrap_or(60);
    let expected_count = (RECENT_WINDOW.num_seconds() / expected_interval).max(1);
    let reporting_cadence = (recent.len() as f64 / expected_count as f64).min(1.0);

    // --- Baseline (Good readings only across baseline_window) ---
    let baseline_cutoff = now - config.baseline_window;
    let baseline_values: Vec<f64> = state
        .history
        .iter()
        .filter(|r| r.timestamp >= baseline_cutoff && r.quality == QualityLevel::Good)
        .map(|r| r.value)
        .collect();

    let recent_good_values: Vec<f64> = recent
        .iter()
        .filter(|r| r.quality == QualityLevel::Good)
        .map(|r| r.value)
        .collect();

    // --- Signals 3 & 4: drift + variance ratio ---
    let (drift, variance_ratio) = if baseline_values.len() < 30 || recent_good_values.is_empty() {
        // Not enough data — assume healthy on these signals rather than
        // penalize sources that simply haven't warmed up yet.
        (1.0, 1.0)
    } else {
        let baseline_mean = mean(&baseline_values);
        let baseline_stddev = stddev(&baseline_values, baseline_mean);
        let recent_mean = mean(&recent_good_values);
        let recent_stddev = stddev(&recent_good_values, recent_mean);

        let drift = if baseline_stddev > 0.0 {
            let z = (recent_mean - baseline_mean).abs() / baseline_stddev;
            (1.0 - (z / 3.0).min(1.0)).max(0.0)
        } else {
            1.0
        };

        let variance = if baseline_stddev > 0.0 {
            let ratio = recent_stddev / baseline_stddev;
            if (0.5..=2.0).contains(&ratio) {
                1.0
            } else if ratio < 0.5 {
                (ratio / 0.5).max(0.0)
            } else {
                (2.0 / ratio).max(0.0)
            }
        } else {
            1.0
        };

        (drift, variance)
    };

    // --- Signal 5: recency ---
    let last_good_reading_at = recent
        .iter()
        .filter(|r| r.quality == QualityLevel::Good)
        .map(|r| r.timestamp)
        .max();

    let recency = match last_good_reading_at {
        Some(ts) => {
            let secs = (now - ts).num_seconds().max(0);
            (1.0 - (secs as f64 / RECENCY_HORIZON.num_seconds() as f64).min(1.0)).max(0.0)
        }
        None => 0.0,
    };

    let overall = (quality_rate * QUALITY_WEIGHT
        + reporting_cadence * CADENCE_WEIGHT
        + drift * DRIFT_WEIGHT
        + variance_ratio * VARIANCE_WEIGHT
        + recency * RECENCY_WEIGHT)
        * 100.0;

    HealthScore {
        overall,
        quality_rate,
        reporting_cadence,
        drift,
        variance_ratio,
        recency,
        readings_in_window: recent.len() as u64,
        last_good_reading_at,
        computed_at: now,
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn stddev(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let variance =
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    variance.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MetricConfig, ValidatorConfig};

    fn cfg() -> ValidatorConfig {
        ValidatorConfig::builder()
            .metric("moisture", MetricConfig::new(0.0..=100.0))
            .build()
    }

    #[test]
    fn empty_state_scores_low() {
        // No readings means quality_rate=0, cadence=0, recency=0.
        // Drift and variance get the "not enough data — assume
        // healthy" fallback so a cold-start source isn't punished for
        // having no baseline yet. Composite lands around 30.
        let s = HealthState::new();
        let score = compute_health_score(&s, &cfg(), None, Utc::now());
        assert_eq!(score.quality_rate, 0.0);
        assert_eq!(score.reporting_cadence, 0.0);
        assert_eq!(score.recency, 0.0);
        assert!(score.overall < 35.0);
    }

    #[test]
    fn all_good_recent_readings_score_well() {
        let now = Utc::now();
        let mut s = HealthState::new();
        // 60 readings, one per minute, ending right at `now` so
        // recency is essentially 1.0.
        for i in 0..60 {
            s.push(
                now - Duration::seconds(60 * (59 - i)),
                42.0 + (i as f64) * 0.01,
                QualityLevel::Good,
                Duration::days(7),
            );
        }
        let cfg = cfg();
        let m = MetricConfig::new(0.0..=100.0);
        let score = compute_health_score(&s, &cfg, Some(&m), now);
        assert!(score.quality_rate > 0.99);
        assert!(score.reporting_cadence > 0.99);
        assert!(score.recency > 0.99);
        assert!(score.overall > 90.0);
    }

    #[test]
    fn half_suspect_drops_quality_rate() {
        let now = Utc::now();
        let mut s = HealthState::new();
        for i in 0..40 {
            let q = if i % 2 == 0 {
                QualityLevel::Good
            } else {
                QualityLevel::Suspect
            };
            s.push(
                now - Duration::seconds(60 * (40 - i)),
                42.0,
                q,
                Duration::days(7),
            );
        }
        let score = compute_health_score(&s, &cfg(), None, now);
        assert!((score.quality_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn drift_penalizes_offset_mean() {
        let now = Utc::now();
        let mut s = HealthState::new();
        // 7 days of baseline at 50.0, then a recent batch at 80.0.
        for i in 0..200 {
            s.push(
                now - Duration::hours(24 * 7) + Duration::minutes(i * 30),
                50.0 + (i as f64).sin() * 0.5,
                QualityLevel::Good,
                Duration::days(7),
            );
        }
        for i in 0..30 {
            s.push(
                now - Duration::minutes(30 - i),
                80.0,
                QualityLevel::Good,
                Duration::days(7),
            );
        }
        let score = compute_health_score(&s, &cfg(), None, now);
        assert!(score.drift < 0.5, "drift was {}", score.drift);
    }

    #[test]
    fn no_recent_data_has_zero_recency() {
        let now = Utc::now();
        let mut s = HealthState::new();
        // Reading older than RECENT_WINDOW.
        s.push(
            now - Duration::hours(3),
            42.0,
            QualityLevel::Good,
            Duration::days(7),
        );
        let score = compute_health_score(&s, &cfg(), None, now);
        assert_eq!(score.recency, 0.0);
    }

    #[test]
    fn state_evicts_outside_baseline_window() {
        let mut s = HealthState::new();
        let now = Utc::now();
        s.push(
            now - Duration::days(8),
            10.0,
            QualityLevel::Good,
            Duration::days(7),
        );
        s.push(now, 20.0, QualityLevel::Good, Duration::days(7));
        // First reading was outside the window relative to the second's
        // insertion — eviction happens on insert.
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history.front().unwrap().value, 20.0);
    }
}
