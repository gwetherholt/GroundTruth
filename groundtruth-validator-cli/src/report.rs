//! Report shape and accumulation. One [`Report`] per metric.

use chrono::{DateTime, Utc};
use groundtruth_validator::{HealthScore, QualityLevel};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub file: String,
    pub metric: String,
    /// Distinct sources observed in the input, sorted.
    pub sources: Vec<String>,
    pub total_readings: u64,
    pub skipped_rows: u64,
    /// First reading's timestamp (after sort), if any.
    pub first_timestamp: Option<DateTime<Utc>>,
    /// Last reading's timestamp (after sort), if any.
    pub last_timestamp: Option<DateTime<Utc>>,
    /// Seconds between first and last timestamp.
    pub duration_seconds: i64,

    pub quality: QualityCounts,
    /// Map of rule name → number of readings that triggered it.
    /// Includes only rules that fired at least once.
    pub flagged_rules: BTreeMap<String, u64>,
    /// Detailed list of every non-Good reading. Used by `full` and
    /// `json` output formats.
    pub flagged_readings: Vec<FlaggedReading>,

    pub health: HealthSummary,
    pub quarantine: QuarantineSummary,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct QualityCounts {
    pub good: u64,
    pub suspect: u64,
    pub invalid: u64,
    pub good_percent: f64,
    pub suspect_percent: f64,
    pub invalid_percent: f64,
}

impl QualityCounts {
    pub fn record(&mut self, q: QualityLevel) {
        match q {
            QualityLevel::Good => self.good += 1,
            QualityLevel::Suspect => self.suspect += 1,
            QualityLevel::Invalid => self.invalid += 1,
        }
    }

    pub fn total(&self) -> u64 {
        self.good + self.suspect + self.invalid
    }

    pub fn finalize(&mut self) {
        let t = self.total() as f64;
        if t > 0.0 {
            self.good_percent = 100.0 * self.good as f64 / t;
            self.suspect_percent = 100.0 * self.suspect as f64 / t;
            self.invalid_percent = 100.0 * self.invalid as f64 / t;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FlaggedReading {
    pub timestamp: DateTime<Utc>,
    pub source: String,
    pub value: f64,
    pub quality: String,
    pub rule: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthSummary {
    pub overall: f64,
    pub status: &'static str,
    pub quality_rate: f64,
    pub reporting_cadence: f64,
    pub drift: f64,
    pub variance_ratio: f64,
    pub recency: f64,
    pub readings_in_window: u64,
}

impl HealthSummary {
    pub fn from_score(score: &HealthScore) -> Self {
        Self {
            overall: score.overall,
            status: status_for_score(score.overall),
            quality_rate: score.quality_rate,
            reporting_cadence: score.reporting_cadence,
            drift: score.drift,
            variance_ratio: score.variance_ratio,
            recency: score.recency,
            readings_in_window: score.readings_in_window,
        }
    }
}

pub fn status_for_score(score: f64) -> &'static str {
    if score >= 90.0 {
        "healthy"
    } else if score >= 70.0 {
        "degraded"
    } else if score >= 40.0 {
        "suspect"
    } else {
        "bad"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QuarantineSummary {
    /// "active" or "quarantined".
    pub status: &'static str,
    pub events: u64,
}

/// Helper accumulator used during the per-row loop. After all readings
/// have been processed, call [`Self::finalize`] to convert into a
/// finished [`Report`].
pub struct ReportBuilder {
    file: String,
    metric: String,
    sources: BTreeSet<String>,
    quality: QualityCounts,
    flagged_rules: BTreeMap<String, u64>,
    flagged_readings: Vec<FlaggedReading>,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    total_readings: u64,
    skipped_rows: u64,
}

impl ReportBuilder {
    pub fn new(file: impl Into<String>, metric: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            metric: metric.into(),
            sources: BTreeSet::new(),
            quality: QualityCounts::default(),
            flagged_rules: BTreeMap::new(),
            flagged_readings: Vec::new(),
            first_timestamp: None,
            last_timestamp: None,
            total_readings: 0,
            skipped_rows: 0,
        }
    }

    pub fn record(
        &mut self,
        source: &str,
        timestamp: DateTime<Utc>,
        value: f64,
        quality: QualityLevel,
        rule: &str,
        reason: &str,
    ) {
        self.total_readings += 1;
        self.sources.insert(source.to_string());
        match self.first_timestamp {
            None => self.first_timestamp = Some(timestamp),
            Some(t) if timestamp < t => self.first_timestamp = Some(timestamp),
            _ => {}
        }
        match self.last_timestamp {
            None => self.last_timestamp = Some(timestamp),
            Some(t) if timestamp > t => self.last_timestamp = Some(timestamp),
            _ => {}
        }
        self.quality.record(quality);
        if quality != QualityLevel::Good {
            if !rule.is_empty() {
                *self.flagged_rules.entry(rule.to_string()).or_default() += 1;
            }
            self.flagged_readings.push(FlaggedReading {
                timestamp,
                source: source.to_string(),
                value,
                quality: quality.as_str().to_string(),
                rule: rule.to_string(),
                reason: reason.to_string(),
            });
        }
    }

    pub fn add_skipped(&mut self, n: usize) {
        self.skipped_rows += n as u64;
    }

    pub fn finalize(mut self, health: HealthSummary, quarantine: QuarantineSummary) -> Report {
        self.quality.finalize();
        let duration_seconds = match (self.first_timestamp, self.last_timestamp) {
            (Some(a), Some(b)) => (b - a).num_seconds().max(0),
            _ => 0,
        };
        Report {
            file: self.file,
            metric: self.metric,
            sources: self.sources.into_iter().collect(),
            total_readings: self.total_readings,
            skipped_rows: self.skipped_rows,
            first_timestamp: self.first_timestamp,
            last_timestamp: self.last_timestamp,
            duration_seconds,
            quality: self.quality,
            flagged_rules: self.flagged_rules,
            flagged_readings: self.flagged_readings,
            health,
            quarantine,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_counts_finalize() {
        let mut q = QualityCounts::default();
        q.record(QualityLevel::Good);
        q.record(QualityLevel::Good);
        q.record(QualityLevel::Suspect);
        q.record(QualityLevel::Invalid);
        q.finalize();
        assert_eq!(q.total(), 4);
        assert!((q.good_percent - 50.0).abs() < 1e-9);
        assert!((q.suspect_percent - 25.0).abs() < 1e-9);
        assert!((q.invalid_percent - 25.0).abs() < 1e-9);
    }

    #[test]
    fn status_thresholds() {
        assert_eq!(status_for_score(95.0), "healthy");
        assert_eq!(status_for_score(80.0), "degraded");
        assert_eq!(status_for_score(50.0), "suspect");
        assert_eq!(status_for_score(20.0), "bad");
    }

    #[test]
    fn builder_tracks_first_last_and_rule_counts() {
        let mut b = ReportBuilder::new("f.csv", "value");
        let t1 = Utc::now();
        let t0 = t1 - chrono::Duration::seconds(60);
        b.record("a", t1, 1.0, QualityLevel::Suspect, "stuck_reading", "x");
        b.record("a", t0, 2.0, QualityLevel::Invalid, "value_range", "y");
        b.record("b", t1, 3.0, QualityLevel::Good, "", "");

        let hs = HealthSummary {
            overall: 80.0,
            status: "degraded",
            quality_rate: 1.0,
            reporting_cadence: 1.0,
            drift: 1.0,
            variance_ratio: 1.0,
            recency: 1.0,
            readings_in_window: 3,
        };
        let qs = QuarantineSummary {
            status: "active",
            events: 0,
        };
        let r = b.finalize(hs, qs);
        assert_eq!(r.total_readings, 3);
        assert_eq!(r.first_timestamp, Some(t0));
        assert_eq!(r.last_timestamp, Some(t1));
        assert_eq!(r.duration_seconds, 60);
        assert_eq!(r.sources, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.quality.good, 1);
        assert_eq!(r.quality.suspect, 1);
        assert_eq!(r.quality.invalid, 1);
        assert_eq!(r.flagged_rules.get("stuck_reading"), Some(&1));
        assert_eq!(r.flagged_rules.get("value_range"), Some(&1));
        assert_eq!(r.flagged_readings.len(), 2);
    }
}
