// Sensor health monitoring (Tier-2 validation).
//
// While Tier-1 (validation.rs) judges individual readings in isolation,
// this module judges sensor STREAMS over time. Each (zone, zone_id,
// metric) gets a 0-100 health score updated periodically.
//
// Five signals feed the score:
//   1. Quality rate (35%) — % of recent readings flagged `good`
//   2. Reporting cadence (25%) — actual vs expected report frequency
//   3. Drift detection (20%) — recent mean's deviation from baseline
//   4. Variance ratio (10%) — recent stddev compared to baseline stddev
//   5. Recency (10%) — time since last good reading
//
// Baselines are computed from the last 7 days of GOOD readings only,
// so that sensor degradation flagged by Tier-1 doesn't infect the
// baseline used to detect further degradation.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::warn;

/// A single sensor stream's identity.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SensorKey {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HealthStatus {
    Healthy,  // 90-100
    Degraded, // 70-89
    Suspect,  // 40-69
    Bad,      // 0-39
}

impl HealthStatus {
    pub fn from_score(score: f64) -> Self {
        if score >= 90.0 {
            HealthStatus::Healthy
        } else if score >= 70.0 {
            HealthStatus::Degraded
        } else if score >= 40.0 {
            HealthStatus::Suspect
        } else {
            HealthStatus::Bad
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthStatus::Healthy => "healthy",
            HealthStatus::Degraded => "degraded",
            HealthStatus::Suspect => "suspect",
            HealthStatus::Bad => "bad",
        }
    }
}

/// All five signals for a sensor, plus the composite score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub key: SensorKey,
    pub score: f64,
    pub status: HealthStatus,
    pub primary_concern: Option<String>,

    // Individual signal scores (0.0 to 1.0)
    pub quality_rate: f64,
    pub cadence: f64,
    pub drift: f64,
    pub variance_ratio: f64,
    pub recency: f64,

    // Diagnostic context
    pub readings_in_window: u64,
    pub last_good_reading_at: Option<DateTime<Utc>>,
    pub computed_at: DateTime<Utc>,
}

const QUALITY_WEIGHT: f64 = 0.35;
const CADENCE_WEIGHT: f64 = 0.25;
const DRIFT_WEIGHT: f64 = 0.20;
const VARIANCE_WEIGHT: f64 = 0.10;
const RECENCY_WEIGHT: f64 = 0.10;

const RECENT_WINDOW_MINUTES: i64 = 60;
const BASELINE_WINDOW_DAYS: i64 = 7;
const RECENCY_HORIZON_SECONDS: i64 = 600;

/// Expected reading interval for cadence calculations, by metric.
fn expected_interval_seconds(metric: &str) -> i64 {
    match metric {
        // Soil sensors: dev firmware publishes every 10s, production
        // every 5min. Use a permissive value that accepts both.
        "moisture" | "moisture_raw" => 60,
        "temperature" | "humidity" => 60,
        _ => 60,
    }
}

pub type SharedHealthCache = Arc<Mutex<HashMap<SensorKey, HealthReport>>>;

pub fn new_cache() -> SharedHealthCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Per-sensor quarantine state. A sensor enters quarantine after a
/// sustained run of bad health scores and exits only after a sustained
/// run of healthy scores. The hysteresis band (40..70) keeps borderline
/// sensors from flapping in and out.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuarantineState {
    pub is_quarantined: bool,
    pub quarantined_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub consecutive_bad_checks: u32,
    pub consecutive_recovery_checks: u32,
}

/// Thresholds for the quarantine state machine.
pub const QUARANTINE_BAD_THRESHOLD: f64 = 40.0;
pub const QUARANTINE_RECOVERY_THRESHOLD: f64 = 70.0;
pub const QUARANTINE_CONSECUTIVE_REQUIRED: u32 = 3;

pub type SharedQuarantineCache = Arc<Mutex<HashMap<SensorKey, QuarantineState>>>;

pub fn new_quarantine_cache() -> SharedQuarantineCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Outcome of a single state-machine step, so callers can react to
/// transitions (e.g. increment a Prometheus counter only on entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineTransition {
    /// Sensor just entered quarantine on this update.
    Entered,
    /// Sensor just exited quarantine on this update.
    Recovered,
    /// No state change.
    Unchanged,
}

/// Apply one observation of a sensor's health score to its quarantine
/// state. Mutates `state` in place and returns the transition (if any).
///
/// Rules:
/// - score < BAD and not quarantined: bump bad counter; quarantine
///   once it hits CONSECUTIVE_REQUIRED. Reset recovery counter.
/// - score >= RECOVERY and quarantined: bump recovery counter;
///   unquarantine once it hits CONSECUTIVE_REQUIRED. Reset bad counter.
/// - score >= BAD and not quarantined: reset bad counter (no progress
///   toward quarantine).
/// - score < RECOVERY and quarantined: reset recovery counter (no
///   progress toward recovery).
pub fn update_quarantine_state(
    state: &mut QuarantineState,
    score: f64,
    now: DateTime<Utc>,
) -> QuarantineTransition {
    if !state.is_quarantined {
        if score < QUARANTINE_BAD_THRESHOLD {
            state.consecutive_bad_checks = state.consecutive_bad_checks.saturating_add(1);
            state.consecutive_recovery_checks = 0;
            if state.consecutive_bad_checks >= QUARANTINE_CONSECUTIVE_REQUIRED {
                state.is_quarantined = true;
                state.quarantined_at = Some(now);
                state.reason = Some(format!(
                    "Health score below {} for {}+ seconds",
                    QUARANTINE_BAD_THRESHOLD as u32,
                    QUARANTINE_CONSECUTIVE_REQUIRED * 30,
                ));
                return QuarantineTransition::Entered;
            }
        } else {
            // Healthy-ish reading while active: any progress toward
            // quarantine resets.
            state.consecutive_bad_checks = 0;
        }
        QuarantineTransition::Unchanged
    } else {
        if score >= QUARANTINE_RECOVERY_THRESHOLD {
            state.consecutive_recovery_checks =
                state.consecutive_recovery_checks.saturating_add(1);
            state.consecutive_bad_checks = 0;
            if state.consecutive_recovery_checks >= QUARANTINE_CONSECUTIVE_REQUIRED {
                *state = QuarantineState::default();
                return QuarantineTransition::Recovered;
            }
        } else {
            // Hysteresis band (or worse): clear recovery progress.
            state.consecutive_recovery_checks = 0;
        }
        QuarantineTransition::Unchanged
    }
}

/// Compute health reports for every sensor that has activity in the
/// last 7 days. Stores results in the shared cache.
pub fn refresh_cache(
    conn: &Connection,
    cache: &SharedHealthCache,
) -> Result<usize, rusqlite::Error> {
    let now = Utc::now();
    let baseline_start = (now - Duration::days(BASELINE_WINDOW_DAYS)).to_rfc3339();

    // Find all active sensor streams.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT zone, zone_id, metric
         FROM readings
         WHERE timestamp >= ?1",
    )?;

    let keys: Vec<SensorKey> = stmt
        .query_map(rusqlite::params![baseline_start], |row| {
            Ok(SensorKey {
                zone: row.get(0)?,
                zone_id: row.get(1)?,
                metric: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    drop(stmt);

    let mut count = 0;
    for key in keys {
        match compute_report(conn, &key, now) {
            Ok(report) => {
                if let Ok(mut c) = cache.lock() {
                    c.insert(key, report);
                    count += 1;
                }
            }
            Err(e) => {
                warn!("Failed to compute health for sensor: {}", e);
            }
        }
    }

    Ok(count)
}

/// Compute the full health report for a single sensor stream.
fn compute_report(
    conn: &Connection,
    key: &SensorKey,
    now: DateTime<Utc>,
) -> Result<HealthReport, rusqlite::Error> {
    let recent_start = (now - Duration::minutes(RECENT_WINDOW_MINUTES)).to_rfc3339();
    let baseline_start = (now - Duration::days(BASELINE_WINDOW_DAYS)).to_rfc3339();

    // --- Recent readings (last 60 min) ---
    let recent_readings =
        fetch_readings_in_range(conn, &key.zone, &key.zone_id, &key.metric, &recent_start)?;

    // --- Baseline readings (last 7 days, GOOD only) ---
    let baseline_readings =
        fetch_good_readings_in_range(conn, &key.zone, &key.zone_id, &key.metric, &baseline_start)?;

    // Signal 1: quality rate
    let quality_rate = if recent_readings.is_empty() {
        0.0
    } else {
        let good_count = recent_readings
            .iter()
            .filter(|r| r.quality == "good")
            .count();
        good_count as f64 / recent_readings.len() as f64
    };

    // Signal 2: reporting cadence
    let expected_interval = expected_interval_seconds(&key.metric);
    let expected_count = (RECENT_WINDOW_MINUTES * 60) / expected_interval;
    let cadence = (recent_readings.len() as f64 / expected_count.max(1) as f64).min(1.0);

    // Signals 3 & 4: drift + variance (require baseline)
    let (drift, variance_ratio) = if baseline_readings.len() < 30 {
        // Not enough baseline data yet — assume healthy for these signals
        (1.0, 1.0)
    } else {
        let baseline_values: Vec<f64> = baseline_readings.iter().map(|r| r.value).collect();
        let recent_good_values: Vec<f64> = recent_readings
            .iter()
            .filter(|r| r.quality == "good")
            .map(|r| r.value)
            .collect();

        if recent_good_values.is_empty() {
            (1.0, 1.0) // can't compute without recent good data
        } else {
            let baseline_mean = mean(&baseline_values);
            let baseline_stddev = stddev(&baseline_values, baseline_mean);
            let recent_mean = mean(&recent_good_values);
            let recent_stddev = stddev(&recent_good_values, recent_mean);

            // Drift: how many stddevs is the recent mean from baseline mean?
            let drift_signal = if baseline_stddev > 0.0 {
                let z = ((recent_mean - baseline_mean).abs()) / baseline_stddev;
                (1.0 - (z / 3.0).min(1.0)).max(0.0)
            } else {
                1.0
            };

            // Variance ratio: 0.5x to 2x is healthy.
            let variance_signal = if baseline_stddev > 0.0 {
                let ratio = recent_stddev / baseline_stddev;
                if (0.5..=2.0).contains(&ratio) {
                    1.0
                } else if ratio < 0.5 {
                    // Too stable — possibly stuck
                    (ratio / 0.5).max(0.0)
                } else {
                    // Too noisy — possibly broken
                    (2.0 / ratio).max(0.0)
                }
            } else {
                1.0
            };

            (drift_signal, variance_signal)
        }
    };

    // Signal 5: recency
    let last_good = recent_readings
        .iter()
        .filter(|r| r.quality == "good")
        .map(|r| r.timestamp)
        .max();

    let recency = match last_good {
        Some(ts) => {
            let secs = (now - ts).num_seconds().max(0);
            (1.0 - (secs as f64 / RECENCY_HORIZON_SECONDS as f64).min(1.0)).max(0.0)
        }
        None => 0.0,
    };

    // Composite score
    let score = (quality_rate * QUALITY_WEIGHT
        + cadence * CADENCE_WEIGHT
        + drift * DRIFT_WEIGHT
        + variance_ratio * VARIANCE_WEIGHT
        + recency * RECENCY_WEIGHT)
        * 100.0;

    let status = HealthStatus::from_score(score);

    // Identify primary concern (signal that contributes least relative to weight)
    let primary_concern =
        identify_primary_concern(quality_rate, cadence, drift, variance_ratio, recency);

    Ok(HealthReport {
        key: key.clone(),
        score,
        status,
        primary_concern,
        quality_rate,
        cadence,
        drift,
        variance_ratio,
        recency,
        readings_in_window: recent_readings.len() as u64,
        last_good_reading_at: last_good,
        computed_at: now,
    })
}

#[derive(Debug, Clone)]
struct ReadingRow {
    value: f64,
    quality: String,
    timestamp: DateTime<Utc>,
}

fn fetch_readings_in_range(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    start: &str,
) -> Result<Vec<ReadingRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT value, quality, timestamp FROM readings
         WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3 AND timestamp >= ?4
         ORDER BY id ASC",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![zone, zone_id, metric, start], |row| {
            let ts_str: String = row.get(2)?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(ReadingRow {
                value: row.get(0)?,
                quality: row.get(1)?,
                timestamp,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

fn fetch_good_readings_in_range(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    start: &str,
) -> Result<Vec<ReadingRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT value, quality, timestamp FROM readings
         WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3
           AND timestamp >= ?4 AND quality = 'good'
         ORDER BY id ASC",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![zone, zone_id, metric, start], |row| {
            let ts_str: String = row.get(2)?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(ReadingRow {
                value: row.get(0)?,
                quality: row.get(1)?,
                timestamp,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
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

fn identify_primary_concern(
    quality: f64,
    cadence: f64,
    drift: f64,
    variance: f64,
    recency: f64,
) -> Option<String> {
    let signals = [
        ("low quality rate", quality, QUALITY_WEIGHT),
        ("missing readings", cadence, CADENCE_WEIGHT),
        ("baseline drift", drift, DRIFT_WEIGHT),
        ("variance anomaly", variance, VARIANCE_WEIGHT),
        ("stale data", recency, RECENCY_WEIGHT),
    ];

    let worst = signals
        .iter()
        .map(|(name, value, weight)| (*name, (1.0 - value) * weight))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    worst.and_then(|(name, severity)| {
        if severity > 0.05 {
            Some(name.to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_thresholds() {
        assert_eq!(HealthStatus::from_score(95.0), HealthStatus::Healthy);
        assert_eq!(HealthStatus::from_score(89.9), HealthStatus::Degraded);
        assert_eq!(HealthStatus::from_score(70.0), HealthStatus::Degraded);
        assert_eq!(HealthStatus::from_score(69.9), HealthStatus::Suspect);
        assert_eq!(HealthStatus::from_score(40.0), HealthStatus::Suspect);
        assert_eq!(HealthStatus::from_score(39.9), HealthStatus::Bad);
        assert_eq!(HealthStatus::from_score(0.0), HealthStatus::Bad);
    }

    #[test]
    fn mean_and_stddev() {
        let values = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let m = mean(&values);
        assert!((m - 5.0).abs() < 0.001);
        let sd = stddev(&values, m);
        assert!((sd - 2.138).abs() < 0.01);
    }

    #[test]
    fn empty_values_safe() {
        assert_eq!(mean(&[]), 0.0);
        assert_eq!(stddev(&[], 0.0), 0.0);
        assert_eq!(stddev(&[5.0], 5.0), 0.0);
    }

    fn run(state: &mut QuarantineState, score: f64) -> QuarantineTransition {
        update_quarantine_state(state, score, Utc::now())
    }

    #[test]
    fn does_not_quarantine_after_one_or_two_bad_checks() {
        let mut s = QuarantineState::default();
        assert_eq!(run(&mut s, 20.0), QuarantineTransition::Unchanged);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 1);

        assert_eq!(run(&mut s, 20.0), QuarantineTransition::Unchanged);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 2);
    }

    #[test]
    fn quarantines_after_three_consecutive_bad_checks() {
        let mut s = QuarantineState::default();
        run(&mut s, 30.0);
        run(&mut s, 30.0);
        let t = run(&mut s, 30.0);
        assert_eq!(t, QuarantineTransition::Entered);
        assert!(s.is_quarantined);
        assert!(s.quarantined_at.is_some());
        assert!(s.reason.as_deref().unwrap().contains("below 40"));
    }

    #[test]
    fn quarantined_sensor_does_not_recover_on_one_or_two_good_checks() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        assert!(s.is_quarantined);

        assert_eq!(run(&mut s, 80.0), QuarantineTransition::Unchanged);
        assert!(s.is_quarantined);
        assert_eq!(s.consecutive_recovery_checks, 1);

        assert_eq!(run(&mut s, 80.0), QuarantineTransition::Unchanged);
        assert!(s.is_quarantined);
        assert_eq!(s.consecutive_recovery_checks, 2);
    }

    #[test]
    fn quarantined_sensor_recovers_after_three_consecutive_good_checks() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        assert!(s.is_quarantined);

        run(&mut s, 75.0);
        run(&mut s, 75.0);
        let t = run(&mut s, 75.0);
        assert_eq!(t, QuarantineTransition::Recovered);
        assert!(!s.is_quarantined);
        assert!(s.quarantined_at.is_none());
        assert!(s.reason.is_none());
        assert_eq!(s.consecutive_bad_checks, 0);
        assert_eq!(s.consecutive_recovery_checks, 0);
    }

    #[test]
    fn hysteresis_score_50_does_not_recover_from_quarantine() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        assert!(s.is_quarantined);

        // 50 is above the bad threshold (40) but below recovery (70).
        // It should not make any recovery progress.
        for _ in 0..10 {
            assert_eq!(run(&mut s, 50.0), QuarantineTransition::Unchanged);
            assert!(s.is_quarantined);
            assert_eq!(s.consecutive_recovery_checks, 0);
        }
    }

    #[test]
    fn bad_counter_resets_when_score_recovers_pre_quarantine() {
        let mut s = QuarantineState::default();
        run(&mut s, 30.0);
        run(&mut s, 30.0);
        assert_eq!(s.consecutive_bad_checks, 2);

        // Any score >= 40 while not yet quarantined resets the bad
        // counter — there's no partial credit toward quarantine.
        run(&mut s, 50.0);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 0);

        // Now a third bad reading alone should NOT trigger quarantine
        // since the streak was broken.
        run(&mut s, 30.0);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 1);
    }

    #[test]
    fn recovery_counter_resets_in_hysteresis_band_after_progress() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        assert!(s.is_quarantined);

        // Two recovery checks, then a dip into the hysteresis band.
        run(&mut s, 80.0);
        run(&mut s, 80.0);
        assert_eq!(s.consecutive_recovery_checks, 2);

        run(&mut s, 60.0); // hysteresis band
        assert!(s.is_quarantined);
        assert_eq!(s.consecutive_recovery_checks, 0);

        // Three more good checks should now be required from scratch.
        run(&mut s, 80.0);
        run(&mut s, 80.0);
        assert!(s.is_quarantined);
        let t = run(&mut s, 80.0);
        assert_eq!(t, QuarantineTransition::Recovered);
        assert!(!s.is_quarantined);
    }

    #[test]
    fn entering_quarantine_zeros_recovery_counter() {
        // Pre-quarantine, a bad reading must reset any stale recovery
        // counter (defense in depth — the field is unused in this state
        // but should not leak in).
        let mut s = QuarantineState {
            consecutive_recovery_checks: 5,
            ..Default::default()
        };
        run(&mut s, 30.0);
        assert_eq!(s.consecutive_recovery_checks, 0);
    }

    #[test]
    fn primary_concern_picks_worst_weighted_signal() {
        // Quality is bad, others are perfect -> "low quality rate"
        let concern = identify_primary_concern(0.0, 1.0, 1.0, 1.0, 1.0);
        assert_eq!(concern, Some("low quality rate".to_string()));

        // All perfect -> no concern
        assert_eq!(identify_primary_concern(1.0, 1.0, 1.0, 1.0, 1.0), None);

        // Cadence is bad, but quality is also slightly off:
        // Quality contributes (1.0 - 0.95) * 0.35 = 0.0175
        // Cadence contributes (1.0 - 0.5) * 0.25 = 0.125
        // Cadence wins.
        let concern = identify_primary_concern(0.95, 0.5, 1.0, 1.0, 1.0);
        assert_eq!(concern, Some("missing readings".to_string()));
    }
}
