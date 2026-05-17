//! `groundtruth-validator` — a two-tier validation framework for
//! streaming numeric data.
//!
//! - **Tier-1** (per-reading rules): value range, raw transducer range,
//!   stuck-reading detection, rate-of-change. See [`tier1`].
//! - **Tier-2** (per-source health scoring): a weighted composite of
//!   quality rate, reporting cadence, baseline drift, variance ratio,
//!   and recency. See [`tier2`].
//! - **Quarantine** state machine with hysteresis. See [`quarantine`].
//!
//! Storage- and transport-agnostic: the validator holds its own
//! in-memory rolling buffers. Persistence, metrics export, and ingest
//! transport (MQTT, HTTP, Kafka, etc.) are caller concerns.
//!
//! See [`StreamValidator`] for the high-level entry point.

pub mod config;
pub mod quarantine;
pub mod reading;
pub mod tier1;
pub mod tier2;
pub mod validator;

pub use config::{MetricConfig, ValidatorConfig, ValidatorConfigBuilder};
pub use quarantine::{update_quarantine, QuarantineState, QuarantineTransition};
pub use reading::{QualityLevel, Reading, ValidationResult};
pub use tier1::{
    check_rate_of_change, check_stuck, run_tier1, validate_raw_range, validate_value_range,
    HistoricalReading, SourceState,
};
pub use tier2::{compute_health_score, HealthScore, HealthState, RecentReading};
pub use validator::{SourceMetric, StreamValidator};
