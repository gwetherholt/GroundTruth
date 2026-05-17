//! Quarantine state machine. A source enters quarantine after a
//! sustained run of low health scores and exits after a sustained run
//! of high scores. The gap between the two thresholds is hysteresis
//! that prevents borderline sources from flapping.

use crate::config::ValidatorConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct QuarantineState {
    pub is_quarantined: bool,
    pub quarantined_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub consecutive_bad_checks: u32,
    pub consecutive_recovery_checks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuarantineTransition {
    /// Source just entered quarantine on this update.
    Entered,
    /// Source just exited quarantine on this update.
    Recovered,
    /// No state change.
    Unchanged,
}

/// Apply one health-score observation to a quarantine state. Mutates
/// `state` in place and returns the transition (if any).
///
/// Rules:
/// - Not quarantined + score < bad: bump bad counter; quarantine once
///   it reaches `quarantine_consecutive_required`. Reset recovery
///   counter.
/// - Quarantined + score >= recovery: bump recovery counter;
///   un-quarantine once it reaches the required count. Reset bad
///   counter.
/// - Not quarantined + score >= bad: reset bad counter (no partial
///   credit toward quarantine).
/// - Quarantined + score < recovery: reset recovery counter (no
///   partial credit toward recovery).
pub fn update_quarantine(
    state: &mut QuarantineState,
    score: f64,
    config: &ValidatorConfig,
    now: DateTime<Utc>,
) -> QuarantineTransition {
    if !state.is_quarantined {
        if score < config.quarantine_bad_threshold {
            state.consecutive_bad_checks = state.consecutive_bad_checks.saturating_add(1);
            state.consecutive_recovery_checks = 0;
            if state.consecutive_bad_checks >= config.quarantine_consecutive_required {
                state.is_quarantined = true;
                state.quarantined_at = Some(now);
                let secs = config.health_check_interval.num_seconds().max(0) as u32
                    * config.quarantine_consecutive_required;
                state.reason = Some(format!(
                    "Health score below {} for {}+ seconds",
                    config.quarantine_bad_threshold as u32, secs,
                ));
                return QuarantineTransition::Entered;
            }
        } else {
            state.consecutive_bad_checks = 0;
        }
        QuarantineTransition::Unchanged
    } else if score >= config.quarantine_recovery_threshold {
        state.consecutive_recovery_checks = state.consecutive_recovery_checks.saturating_add(1);
        state.consecutive_bad_checks = 0;
        if state.consecutive_recovery_checks >= config.quarantine_consecutive_required {
            *state = QuarantineState::default();
            return QuarantineTransition::Recovered;
        }
        QuarantineTransition::Unchanged
    } else {
        state.consecutive_recovery_checks = 0;
        QuarantineTransition::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ValidatorConfig;

    fn cfg() -> ValidatorConfig {
        ValidatorConfig::builder().build()
    }

    fn run(state: &mut QuarantineState, score: f64) -> QuarantineTransition {
        update_quarantine(state, score, &cfg(), Utc::now())
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

        run(&mut s, 80.0);
        assert!(s.is_quarantined);
        assert_eq!(s.consecutive_recovery_checks, 1);

        run(&mut s, 80.0);
        assert!(s.is_quarantined);
        assert_eq!(s.consecutive_recovery_checks, 2);
    }

    #[test]
    fn quarantined_sensor_recovers_after_three_good_checks() {
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
        assert_eq!(s.consecutive_bad_checks, 0);
        assert_eq!(s.consecutive_recovery_checks, 0);
    }

    #[test]
    fn hysteresis_band_blocks_recovery() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        assert!(s.is_quarantined);

        for _ in 0..10 {
            assert_eq!(run(&mut s, 50.0), QuarantineTransition::Unchanged);
            assert!(s.is_quarantined);
            assert_eq!(s.consecutive_recovery_checks, 0);
        }
    }

    #[test]
    fn bad_counter_resets_pre_quarantine() {
        let mut s = QuarantineState::default();
        run(&mut s, 30.0);
        run(&mut s, 30.0);
        run(&mut s, 50.0);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 0);

        run(&mut s, 30.0);
        assert!(!s.is_quarantined);
        assert_eq!(s.consecutive_bad_checks, 1);
    }

    #[test]
    fn recovery_counter_resets_in_hysteresis_band() {
        let mut s = QuarantineState::default();
        for _ in 0..3 {
            run(&mut s, 30.0);
        }
        run(&mut s, 80.0);
        run(&mut s, 80.0);
        assert_eq!(s.consecutive_recovery_checks, 2);

        run(&mut s, 60.0);
        assert_eq!(s.consecutive_recovery_checks, 0);
        assert!(s.is_quarantined);
    }

    #[test]
    fn custom_thresholds_via_config() {
        let cfg = ValidatorConfig::builder()
            .quarantine_bad_threshold(20.0)
            .quarantine_recovery_threshold(60.0)
            .quarantine_consecutive_required(2)
            .build();
        let mut s = QuarantineState::default();
        update_quarantine(&mut s, 15.0, &cfg, Utc::now());
        let t = update_quarantine(&mut s, 15.0, &cfg, Utc::now());
        assert_eq!(t, QuarantineTransition::Entered);
        update_quarantine(&mut s, 65.0, &cfg, Utc::now());
        let t = update_quarantine(&mut s, 65.0, &cfg, Utc::now());
        assert_eq!(t, QuarantineTransition::Recovered);
    }
}
