use anyhow::{anyhow, Result};
use chrono::Duration;
use clap::{Parser, ValueEnum};
use std::ops::RangeInclusive;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "gt-validate",
    version,
    about = "Run groundtruth-validator over a CSV file"
)]
pub struct Args {
    /// Path to CSV file. Use "-" for stdin.
    pub file: String,

    /// Metric name to assign. If omitted and the CSV has multiple
    /// numeric value columns, each is processed as a separate metric.
    #[arg(long)]
    pub metric: Option<String>,

    /// Source name to assign if the CSV has no source column.
    #[arg(long, default_value = "default")]
    pub source: String,

    /// Valid value range, e.g. "0:100" or "-40:200".
    #[arg(long, value_parser = parse_value_range)]
    pub range: Option<RangeInclusive<f64>>,

    /// Valid raw transducer range, e.g. "100:3995".
    #[arg(long = "raw-range", value_parser = parse_raw_range)]
    pub raw_range: Option<RangeInclusive<i64>>,

    /// Expected reporting cadence, e.g. "30s" or "5m" or "1h".
    #[arg(long, default_value = "60s", value_parser = parse_duration)]
    pub cadence: Duration,

    /// Consecutive readings within stuck-threshold to flag as stuck.
    #[arg(long = "stuck-count", default_value_t = 6)]
    pub stuck_count: usize,

    /// Max absolute change between consecutive readings.
    #[arg(long = "max-rate", default_value_t = 10.0)]
    pub max_rate: f64,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Summary)]
    pub format: OutputFormat,

    /// Disable colored output (also respected via NO_COLOR env var).
    #[arg(long = "no-color")]
    pub no_color: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Summary,
    Full,
    Json,
}

/// Parse a "min:max" range of f64. Reject NaN, require min <= max.
pub fn parse_value_range(s: &str) -> Result<RangeInclusive<f64>, String> {
    let (lo, hi) = s
        .split_once(':')
        .ok_or_else(|| format!("expected MIN:MAX, got '{}'", s))?;
    let lo: f64 = lo
        .trim()
        .parse()
        .map_err(|e| format!("invalid min '{}': {}", lo, e))?;
    let hi: f64 = hi
        .trim()
        .parse()
        .map_err(|e| format!("invalid max '{}': {}", hi, e))?;
    if lo.is_nan() || hi.is_nan() {
        return Err("range bounds must not be NaN".into());
    }
    if lo > hi {
        return Err(format!("min ({}) must be <= max ({})", lo, hi));
    }
    Ok(lo..=hi)
}

pub fn parse_raw_range(s: &str) -> Result<RangeInclusive<i64>, String> {
    let (lo, hi) = s
        .split_once(':')
        .ok_or_else(|| format!("expected MIN:MAX, got '{}'", s))?;
    let lo: i64 = lo
        .trim()
        .parse()
        .map_err(|e| format!("invalid min '{}': {}", lo, e))?;
    let hi: i64 = hi
        .trim()
        .parse()
        .map_err(|e| format!("invalid max '{}': {}", hi, e))?;
    if lo > hi {
        return Err(format!("min ({}) must be <= max ({})", lo, hi));
    }
    Ok(lo..=hi)
}

/// Parse a short duration string like `30s`, `5m`, `1h`, `1d`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    parse_duration_inner(s).map_err(|e| e.to_string())
}

fn parse_duration_inner(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| c.is_ascii_alphabetic())
        .map(|i| s.split_at(i))
        .ok_or_else(|| anyhow!("expected NUMBER+UNIT (e.g. '30s'), got '{}'", s))?;
    let n: i64 = num
        .trim()
        .parse()
        .map_err(|e| anyhow!("invalid number '{}': {}", num, e))?;
    if n < 0 {
        return Err(anyhow!("duration must be non-negative, got {}", n));
    }
    let d = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        other => return Err(anyhow!("unknown duration unit '{}', use s/m/h/d", other)),
    };
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_basic() {
        let r = parse_value_range("0:100").unwrap();
        assert_eq!(*r.start(), 0.0);
        assert_eq!(*r.end(), 100.0);
    }

    #[test]
    fn parse_range_negative() {
        let r = parse_value_range("-40:200").unwrap();
        assert_eq!(*r.start(), -40.0);
        assert_eq!(*r.end(), 200.0);
    }

    #[test]
    fn parse_range_rejects_inverted() {
        assert!(parse_value_range("10:5").is_err());
    }

    #[test]
    fn parse_range_rejects_missing_colon() {
        assert!(parse_value_range("0-100").is_err());
    }

    #[test]
    fn parse_raw_range_basic() {
        let r = parse_raw_range("100:3995").unwrap();
        assert_eq!(*r.start(), 100);
        assert_eq!(*r.end(), 3995);
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::seconds(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::minutes(5));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("10y").is_err());
        assert!(parse_duration("foo").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn parse_duration_rejects_negative() {
        assert!(parse_duration("-30s").is_err());
    }
}
