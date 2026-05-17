use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A single observation flowing through the validator. The validator
/// doesn't know or care whether `source` is a soil sensor, a server
/// hostname, or a financial ticker — it's just an identity for the
/// stream the reading belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reading {
    /// Stream identity (e.g. `"bed/1"`, `"host-42"`, `"AAPL"`).
    pub source: String,
    /// What is being measured on this stream (e.g. `"moisture"`,
    /// `"cpu_pct"`, `"price"`).
    pub metric: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
    /// Optional raw reading (e.g. ADC counts) for sources whose
    /// calibrated value is derived from a raw transducer output.
    pub raw_value: Option<i64>,
}

impl Reading {
    pub fn new(
        source: impl Into<String>,
        metric: impl Into<String>,
        value: f64,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            source: source.into(),
            metric: metric.into(),
            value,
            timestamp,
            raw_value: None,
        }
    }

    pub fn with_raw(mut self, raw: i64) -> Self {
        self.raw_value = Some(raw);
        self
    }
}

/// Severity classification. Ord is derived so that
/// `Invalid > Suspect > Good` — a reading's quality can be safely taken
/// as the `max` of multiple rule outcomes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum QualityLevel {
    Good,
    Suspect,
    Invalid,
}

impl QualityLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            QualityLevel::Good => "good",
            QualityLevel::Suspect => "suspect",
            QualityLevel::Invalid => "invalid",
        }
    }
}

impl fmt::Display for QualityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The verdict from running the Tier-1 pipeline on one reading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationResult {
    pub quality: QualityLevel,
    /// Human-readable explanation. Empty when `quality == Good`.
    pub reason: String,
    /// Identifier of the rule that produced this verdict (e.g.
    /// `"value_range"`, `"stuck_reading"`). Empty when no rule fired.
    pub rule: String,
}

impl ValidationResult {
    pub fn good() -> Self {
        Self {
            quality: QualityLevel::Good,
            reason: String::new(),
            rule: String::new(),
        }
    }

    pub fn suspect(rule: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            quality: QualityLevel::Suspect,
            reason: reason.into(),
            rule: rule.into(),
        }
    }

    pub fn invalid(rule: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            quality: QualityLevel::Invalid,
            reason: reason.into(),
            rule: rule.into(),
        }
    }

    /// Severity-preserving merge: keep the more-severe verdict. If both
    /// have the same severity, the first wins. Used by the pipeline to
    /// fold individual rule outcomes into a single result.
    pub fn merge(self, other: ValidationResult) -> ValidationResult {
        if other.quality > self.quality {
            other
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_level_ordering() {
        assert!(QualityLevel::Good < QualityLevel::Suspect);
        assert!(QualityLevel::Suspect < QualityLevel::Invalid);
        assert!(QualityLevel::Invalid > QualityLevel::Good);
    }

    #[test]
    fn merge_keeps_more_severe() {
        let g = ValidationResult::good();
        let s = ValidationResult::suspect("r1", "slow");
        let i = ValidationResult::invalid("r2", "out of range");

        assert_eq!(g.clone().merge(s.clone()).quality, QualityLevel::Suspect);
        assert_eq!(s.clone().merge(g.clone()).quality, QualityLevel::Suspect);
        assert_eq!(s.clone().merge(i.clone()).quality, QualityLevel::Invalid);
        assert_eq!(i.clone().merge(s).quality, QualityLevel::Invalid);
        assert_eq!(g.clone().merge(g).quality, QualityLevel::Good);
    }
}
