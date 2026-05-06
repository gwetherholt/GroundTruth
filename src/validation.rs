use rusqlite::Connection;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Good,
    Suspect,
    Invalid,
}

impl Quality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Suspect => "suspect",
            Quality::Invalid => "invalid",
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    pub quality: Quality,
    pub reason: Option<String>,
}

impl Validation {
    pub fn good() -> Self {
        Self {
            quality: Quality::Good,
            reason: None,
        }
    }
    pub fn suspect(reason: impl Into<String>) -> Self {
        Self {
            quality: Quality::Suspect,
            reason: Some(reason.into()),
        }
    }
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            quality: Quality::Invalid,
            reason: Some(reason.into()),
        }
    }
}

pub mod limits {
    pub const RAW_ADC_MIN: i64 = 100;
    pub const RAW_ADC_MAX: i64 = 3995;

    pub const MOISTURE_PCT_MIN: f64 = 0.0;
    pub const MOISTURE_PCT_MAX: f64 = 100.0;
    pub const HUMIDITY_PCT_MIN: f64 = 0.0;
    pub const HUMIDITY_PCT_MAX: f64 = 100.0;
    pub const TEMP_F_MIN: f64 = -40.0;
    pub const TEMP_F_MAX: f64 = 200.0;

    pub const STUCK_CONSECUTIVE_COUNT: usize = 6;
    pub const STUCK_TOLERANCE: f64 = 0.01;

    pub const MOISTURE_DELTA_THRESHOLD: f64 = 30.0;
    pub const HUMIDITY_DELTA_THRESHOLD: f64 = 30.0;
    pub const TEMP_DELTA_THRESHOLD: f64 = 20.0;
    pub const RATE_WINDOW_SECONDS: i64 = 600;
}

pub fn validate_value_range(metric: &str, value: f64) -> Validation {
    use limits::*;
    let (min, max) = match metric {
        "moisture" => (MOISTURE_PCT_MIN, MOISTURE_PCT_MAX),
        "humidity" => (HUMIDITY_PCT_MIN, HUMIDITY_PCT_MAX),
        "temperature" => (TEMP_F_MIN, TEMP_F_MAX),
        _ => return Validation::good(),
    };

    if value.is_nan() {
        return Validation::invalid("value is NaN");
    }
    if value < min || value > max {
        return Validation::invalid(format!(
            "{} = {:.2} outside plausible range [{}, {}]",
            metric, value, min, max
        ));
    }
    Validation::good()
}

pub fn validate_raw_adc(raw: i64) -> Validation {
    use limits::*;
    if !(RAW_ADC_MIN..=RAW_ADC_MAX).contains(&raw) {
        return Validation::invalid(format!(
            "raw ADC = {} outside plausible range [{}, {}]",
            raw, RAW_ADC_MIN, RAW_ADC_MAX
        ));
    }
    Validation::good()
}

pub fn check_stuck(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    new_value: f64,
) -> Validation {
    use limits::*;

    let mut stmt = match conn.prepare(
        "SELECT value FROM readings
         WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3
         ORDER BY id DESC
         LIMIT ?4",
    ) {
        Ok(s) => s,
        Err(_) => return Validation::good(),
    };

    let recent: Vec<f64> = stmt
        .query_map(
            rusqlite::params![zone, zone_id, metric, (STUCK_CONSECUTIVE_COUNT - 1) as i64],
            |row| row.get(0),
        )
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    if recent.len() < STUCK_CONSECUTIVE_COUNT - 1 {
        return Validation::good();
    }

    let all_match = recent
        .iter()
        .all(|&v| (v - new_value).abs() <= STUCK_TOLERANCE);
    if all_match {
        Validation::suspect(format!(
            "{} consecutive readings within {} of {:.2} — sensor may be stuck",
            STUCK_CONSECUTIVE_COUNT, STUCK_TOLERANCE, new_value
        ))
    } else {
        Validation::good()
    }
}

pub fn check_rate_of_change(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    new_value: f64,
    new_timestamp_rfc3339: &str,
) -> Validation {
    use limits::*;

    let threshold = match metric {
        "moisture" => MOISTURE_DELTA_THRESHOLD,
        "humidity" => HUMIDITY_DELTA_THRESHOLD,
        "temperature" => TEMP_DELTA_THRESHOLD,
        _ => return Validation::good(),
    };

    let last: Option<(f64, String)> = conn
        .query_row(
            "SELECT value, timestamp FROM readings
             WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![zone, zone_id, metric],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    let (last_value, last_ts_str) = match last {
        Some(p) => p,
        None => return Validation::good(),
    };

    let last_ts = match chrono::DateTime::parse_from_rfc3339(&last_ts_str) {
        Ok(dt) => dt,
        Err(_) => return Validation::good(),
    };
    let new_ts = match chrono::DateTime::parse_from_rfc3339(new_timestamp_rfc3339) {
        Ok(dt) => dt,
        Err(_) => return Validation::good(),
    };

    let dt_secs = (new_ts - last_ts).num_seconds();
    if dt_secs <= 0 || dt_secs > RATE_WINDOW_SECONDS {
        return Validation::good();
    }

    let delta = (new_value - last_value).abs();
    if delta > threshold {
        Validation::suspect(format!(
            "{} changed by {:.1} in {}s (threshold {:.1}/{}s)",
            metric, delta, dt_secs, threshold, RATE_WINDOW_SECONDS
        ))
    } else {
        Validation::good()
    }
}

pub fn validate_reading(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    value: f64,
    raw_adc: Option<i64>,
    timestamp_rfc3339: &str,
) -> Validation {
    let mut current = Validation::good();

    let merge = |acc: Validation, next: Validation| -> Validation {
        match (acc.quality, next.quality) {
            (Quality::Invalid, _) => acc,
            (_, Quality::Invalid) => next,
            (Quality::Suspect, Quality::Suspect) => Validation::suspect(format!(
                "{}; {}",
                acc.reason.unwrap_or_default(),
                next.reason.unwrap_or_default()
            )),
            (Quality::Suspect, _) => acc,
            (_, Quality::Suspect) => next,
            _ => Validation::good(),
        }
    };

    current = merge(current, validate_value_range(metric, value));
    if current.quality == Quality::Invalid {
        return current;
    }

    if let Some(raw) = raw_adc {
        current = merge(current, validate_raw_adc(raw));
        if current.quality == Quality::Invalid {
            return current;
        }
    }

    current = merge(current, check_stuck(conn, zone, zone_id, metric, value));
    current = merge(
        current,
        check_rate_of_change(conn, zone, zone_id, metric, value, timestamp_rfc3339),
    );

    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::create_schema(&conn).unwrap();
        conn
    }

    fn ts(seconds_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(seconds_ago)).to_rfc3339()
    }

    #[test]
    fn moisture_in_range_is_good() {
        assert_eq!(
            validate_value_range("moisture", 42.5).quality,
            Quality::Good
        );
    }

    #[test]
    fn moisture_above_100_is_invalid() {
        assert_eq!(
            validate_value_range("moisture", 105.0).quality,
            Quality::Invalid
        );
    }

    #[test]
    fn moisture_negative_is_invalid() {
        assert_eq!(
            validate_value_range("moisture", -5.0).quality,
            Quality::Invalid
        );
    }

    #[test]
    fn temperature_extreme_cold_is_invalid() {
        assert_eq!(
            validate_value_range("temperature", -100.0).quality,
            Quality::Invalid
        );
    }

    #[test]
    fn temperature_normal_is_good() {
        assert_eq!(
            validate_value_range("temperature", 72.0).quality,
            Quality::Good
        );
    }

    #[test]
    fn nan_is_invalid() {
        assert_eq!(
            validate_value_range("moisture", f64::NAN).quality,
            Quality::Invalid
        );
    }

    #[test]
    fn raw_adc_in_range_is_good() {
        assert_eq!(validate_raw_adc(2400).quality, Quality::Good);
    }

    #[test]
    fn raw_adc_zero_is_invalid() {
        assert_eq!(validate_raw_adc(0).quality, Quality::Invalid);
    }

    #[test]
    fn raw_adc_max_is_invalid() {
        assert_eq!(validate_raw_adc(4095).quality, Quality::Invalid);
    }

    #[test]
    fn stuck_six_identical_readings_is_suspect() {
        let conn = fresh_db();
        for i in 0..5 {
            db::insert_reading(
                &conn,
                "bed",
                "1",
                "moisture",
                42.5,
                &ts(60 * (5 - i)),
                None,
                "good",
                None,
            )
            .unwrap();
        }
        let v = check_stuck(&conn, "bed", "1", "moisture", 42.5);
        assert_eq!(v.quality, Quality::Suspect);
    }

    #[test]
    fn stuck_with_recent_variation_is_good() {
        let conn = fresh_db();
        let values = [42.5, 42.5, 42.5, 43.0, 42.5];
        for (i, v) in values.iter().enumerate() {
            db::insert_reading(
                &conn,
                "bed",
                "1",
                "moisture",
                *v,
                &ts(60 * (5 - i as i64)),
                None,
                "good",
                None,
            )
            .unwrap();
        }
        let v = check_stuck(&conn, "bed", "1", "moisture", 42.5);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn stuck_insufficient_history_is_good() {
        let conn = fresh_db();
        db::insert_reading(
            &conn,
            "bed",
            "1",
            "moisture",
            42.5,
            &ts(60),
            None,
            "good",
            None,
        )
        .unwrap();
        let v = check_stuck(&conn, "bed", "1", "moisture", 42.5);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn rapid_moisture_jump_is_suspect() {
        let conn = fresh_db();
        let earlier = ts(60);
        let now = chrono::Utc::now().to_rfc3339();
        db::insert_reading(
            &conn, "bed", "1", "moisture", 20.0, &earlier, None, "good", None,
        )
        .unwrap();
        let v = check_rate_of_change(&conn, "bed", "1", "moisture", 80.0, &now);
        assert_eq!(v.quality, Quality::Suspect);
    }

    #[test]
    fn small_moisture_change_is_good() {
        let conn = fresh_db();
        let earlier = ts(60);
        let now = chrono::Utc::now().to_rfc3339();
        db::insert_reading(
            &conn, "bed", "1", "moisture", 40.0, &earlier, None, "good", None,
        )
        .unwrap();
        let v = check_rate_of_change(&conn, "bed", "1", "moisture", 42.0, &now);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn change_outside_window_is_good() {
        let conn = fresh_db();
        let earlier = ts(3600);
        let now = chrono::Utc::now().to_rfc3339();
        db::insert_reading(
            &conn, "bed", "1", "moisture", 20.0, &earlier, None, "good", None,
        )
        .unwrap();
        let v = check_rate_of_change(&conn, "bed", "1", "moisture", 80.0, &now);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn no_prior_reading_is_good() {
        let conn = fresh_db();
        let now = chrono::Utc::now().to_rfc3339();
        let v = check_rate_of_change(&conn, "bed", "1", "moisture", 80.0, &now);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn fully_valid_reading_is_good() {
        let conn = fresh_db();
        let now = chrono::Utc::now().to_rfc3339();
        let v = validate_reading(&conn, "bed", "1", "moisture", 42.5, Some(2400), &now);
        assert_eq!(v.quality, Quality::Good);
    }

    #[test]
    fn invalid_value_short_circuits_subsequent_rules() {
        let conn = fresh_db();
        let now = chrono::Utc::now().to_rfc3339();
        let v = validate_reading(&conn, "bed", "1", "moisture", 200.0, Some(2400), &now);
        assert_eq!(v.quality, Quality::Invalid);
    }

    #[test]
    fn good_value_with_invalid_raw_adc_is_invalid() {
        let conn = fresh_db();
        let now = chrono::Utc::now().to_rfc3339();
        let v = validate_reading(&conn, "bed", "1", "moisture", 50.0, Some(0), &now);
        assert_eq!(v.quality, Quality::Invalid);
    }
}
