//! CSV ingestion. The CLI accepts arbitrary CSVs, auto-detecting which
//! columns are timestamps, sources, values, or raw transducer readings.
//!
//! Detection is case-insensitive and uses these heuristics:
//! - **Timestamp**: header matches `timestamp` / `time` / `datetime` /
//!   `date` / `ts`.
//! - **Source**: header matches `source` / `sensor` / `device` /
//!   `node` / `id`.
//! - **Raw value**: header matches `raw` / `raw_value` / `adc`.
//! - **Value columns**: every remaining column whose data is numeric.
//!
//! If no timestamp column is present, synthetic timestamps are
//! generated starting at "now" and advancing by the configured cadence.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use groundtruth_validator::Reading;
use std::io::Read;
use std::path::Path;

/// Lowercased header aliases for each detected role.
const TIMESTAMP_ALIASES: &[&str] = &["timestamp", "time", "datetime", "date", "ts"];
const SOURCE_ALIASES: &[&str] = &["source", "sensor", "device", "node", "id"];
const RAW_ALIASES: &[&str] = &["raw", "raw_value", "adc"];
/// Headers that suggest this column is the primary value (so a CSV with
/// e.g. `temperature,raw` doesn't end up treating `temperature` as
/// "just another numeric column" — though detection still works either
/// way since both unnamed and named value columns are auto-promoted).
const VALUE_ALIASES: &[&str] = &["value", "reading", "measurement", "data"];

/// The role assigned to each input column after detection.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnRole {
    Timestamp,
    Source,
    RawValue,
    /// A numeric value column. The metric name is the original header.
    Value { metric: String },
    /// A column we couldn't make sense of — ignored.
    Ignored,
}

/// Result of inspecting a CSV's header + first data row.
#[derive(Debug, Clone)]
pub struct DetectedSchema {
    pub headers: Vec<String>,
    pub roles: Vec<ColumnRole>,
}

impl DetectedSchema {
    pub fn timestamp_idx(&self) -> Option<usize> {
        self.roles.iter().position(|r| matches!(r, ColumnRole::Timestamp))
    }

    pub fn source_idx(&self) -> Option<usize> {
        self.roles.iter().position(|r| matches!(r, ColumnRole::Source))
    }

    pub fn raw_idx(&self) -> Option<usize> {
        self.roles.iter().position(|r| matches!(r, ColumnRole::RawValue))
    }

    pub fn value_columns(&self) -> Vec<(usize, String)> {
        self.roles
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                ColumnRole::Value { metric } => Some((i, metric.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Inspect headers + a sample of rows to assign a role to each column.
/// `sample_rows` should be a small slice (one row is enough in practice);
/// we use it only to confirm "numeric-looking" columns.
pub fn detect_schema(headers: &[String], sample_rows: &[Vec<String>]) -> DetectedSchema {
    let mut roles: Vec<ColumnRole> = headers
        .iter()
        .map(|h| classify_header(h))
        .collect();

    // Promote numeric-looking unassigned columns to Value, using the
    // header as the metric name.
    for (i, role) in roles.iter_mut().enumerate() {
        if matches!(role, ColumnRole::Ignored) && column_is_numeric(sample_rows, i) {
            let metric = headers
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("col_{}", i));
            *role = ColumnRole::Value { metric };
        }
    }

    DetectedSchema {
        headers: headers.to_vec(),
        roles,
    }
}

fn classify_header(header: &str) -> ColumnRole {
    let h = header.trim().to_lowercase();
    if TIMESTAMP_ALIASES.iter().any(|a| *a == h) {
        return ColumnRole::Timestamp;
    }
    if SOURCE_ALIASES.iter().any(|a| *a == h) {
        return ColumnRole::Source;
    }
    if RAW_ALIASES.iter().any(|a| *a == h) {
        return ColumnRole::RawValue;
    }
    if VALUE_ALIASES.iter().any(|a| *a == h) {
        return ColumnRole::Value { metric: h };
    }
    ColumnRole::Ignored
}

fn column_is_numeric(rows: &[Vec<String>], idx: usize) -> bool {
    if rows.is_empty() {
        return false;
    }
    let mut saw_one = false;
    for row in rows {
        if let Some(cell) = row.get(idx) {
            let s = cell.trim();
            if s.is_empty() {
                continue;
            }
            if s.parse::<f64>().is_ok() {
                saw_one = true;
            } else {
                return false;
            }
        }
    }
    saw_one
}

/// One parsed reading scoped to a single metric column, before the
/// validator runs. We preserve the original row index so error messages
/// stay useful.
#[derive(Debug, Clone)]
pub struct ParsedReading {
    pub reading: Reading,
}

/// Open a CSV from a path or stdin (when `path` is `"-"`).
pub fn open_reader(path: &str) -> Result<Box<dyn Read>> {
    if path == "-" {
        Ok(Box::new(std::io::stdin()))
    } else {
        let p = Path::new(path);
        let f = std::fs::File::open(p)
            .with_context(|| format!("failed to open '{}'", path))?;
        Ok(Box::new(f))
    }
}

/// Read the full CSV into headers + rows. Trims BOMs and skips blank
/// lines silently.
pub fn read_csv<R: Read>(reader: R) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);

    let headers: Vec<String> = rdr
        .headers()
        .context("failed to read CSV headers")?
        .iter()
        .map(|h| h.trim().trim_start_matches('\u{feff}').to_string())
        .collect();

    if headers.is_empty() {
        bail!("CSV has no header row");
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.with_context(|| format!("failed to parse row {}", i + 1))?;
        rows.push(rec.iter().map(|s| s.to_string()).collect());
    }

    Ok((headers, rows))
}

/// Parse a timestamp cell. Tries RFC3339 first, then a few common
/// patterns, then Unix epoch seconds / milliseconds.
pub fn parse_timestamp(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty timestamp");
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    for fmt in &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(nd) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(Utc.from_utc_datetime(&nd));
        }
        // Date-only patterns parse as NaiveDate; try them as midnight UTC.
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
            let nd = d.and_hms_opt(0, 0, 0).unwrap();
            return Ok(Utc.from_utc_datetime(&nd));
        }
    }
    if let Ok(secs) = s.parse::<i64>() {
        // Heuristic: treat anything > 10^12 as milliseconds.
        let (s_part, ns_part) = if secs.abs() > 1_000_000_000_000 {
            (secs / 1000, ((secs % 1000) as u32) * 1_000_000)
        } else {
            (secs, 0)
        };
        if let Some(dt) = chrono::DateTime::<Utc>::from_timestamp(s_part, ns_part) {
            return Ok(dt);
        }
    }
    Err(anyhow!("unrecognized timestamp format: '{}'", s))
}

/// Synthesize a timestamp for the `i`-th row when the CSV has none.
/// Starts at `start` and advances by `cadence` for each subsequent row.
pub fn synthetic_timestamp(start: DateTime<Utc>, i: usize, cadence: Duration) -> DateTime<Utc> {
    start + cadence * (i as i32)
}

/// Build a list of [`Reading`] structs for one specific metric column,
/// given the parsed CSV and detected schema.
///
/// - `value_col_idx` is the column index of the metric whose values are
///   being extracted.
/// - `value_col_name` is the metric name to attach to each Reading.
/// - `default_source` is used when no source column is detected.
/// - `synthetic_start` and `cadence` are used when no timestamp column
///   is detected.
///
/// Rows where the value cell is blank or non-numeric are skipped (with
/// a count returned alongside, so the caller can report it).
pub fn build_readings(
    headers: &[String],
    rows: &[Vec<String>],
    schema: &DetectedSchema,
    value_col_idx: usize,
    value_col_name: &str,
    default_source: &str,
    synthetic_start: DateTime<Utc>,
    cadence: Duration,
) -> Result<(Vec<Reading>, usize)> {
    let _ = headers; // currently only used implicitly via schema
    let ts_idx = schema.timestamp_idx();
    let src_idx = schema.source_idx();
    let raw_idx = schema.raw_idx();

    let mut readings = Vec::with_capacity(rows.len());
    let mut skipped = 0usize;

    for (i, row) in rows.iter().enumerate() {
        let raw_value = row.get(value_col_idx).map(|s| s.trim()).unwrap_or("");
        if raw_value.is_empty() {
            skipped += 1;
            continue;
        }
        let value: f64 = match raw_value.parse() {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let timestamp = match ts_idx {
            Some(idx) => {
                let cell = row.get(idx).map(|s| s.trim()).unwrap_or("");
                parse_timestamp(cell)
                    .with_context(|| format!("row {}: bad timestamp '{}'", i + 1, cell))?
            }
            None => synthetic_timestamp(synthetic_start, i, cadence),
        };

        let source = match src_idx {
            Some(idx) => row
                .get(idx)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default_source.to_string()),
            None => default_source.to_string(),
        };

        let raw_value_field = raw_idx.and_then(|idx| {
            row.get(idx)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<i64>().ok())
        });

        let mut r = Reading::new(source, value_col_name, value, timestamp);
        if let Some(raw) = raw_value_field {
            r = r.with_raw(raw);
        }
        readings.push(r);
    }

    readings.sort_by_key(|r| r.timestamp);
    Ok((readings, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn classify_timestamp_aliases() {
        for h in ["timestamp", "Time", "DATETIME", "ts", "date"] {
            assert!(matches!(classify_header(h), ColumnRole::Timestamp), "{}", h);
        }
    }

    #[test]
    fn classify_source_aliases() {
        for h in ["source", "Sensor", "DEVICE", "node", "id"] {
            assert!(matches!(classify_header(h), ColumnRole::Source), "{}", h);
        }
    }

    #[test]
    fn classify_raw_aliases() {
        for h in ["raw", "Raw_Value", "ADC"] {
            assert!(matches!(classify_header(h), ColumnRole::RawValue), "{}", h);
        }
    }

    #[test]
    fn classify_value_named_alias() {
        match classify_header("Value") {
            ColumnRole::Value { metric } => assert_eq!(metric, "value"),
            r => panic!("expected Value, got {:?}", r),
        }
    }

    #[test]
    fn unknown_header_is_ignored_but_promoted_if_numeric() {
        let headers = vec!["x".to_string()];
        let rows = vec![vec!["1.5".to_string()], vec!["2.5".to_string()]];
        let schema = detect_schema(&headers, &rows);
        match &schema.roles[0] {
            ColumnRole::Value { metric } => assert_eq!(metric, "x"),
            r => panic!("expected Value, got {:?}", r),
        }
    }

    #[test]
    fn non_numeric_unknown_stays_ignored() {
        let headers = vec!["note".to_string()];
        let rows = vec![vec!["hi".to_string()]];
        let schema = detect_schema(&headers, &rows);
        assert_eq!(schema.roles[0], ColumnRole::Ignored);
    }

    #[test]
    fn detect_full_schema() {
        let headers = vec![
            "timestamp".to_string(),
            "sensor".to_string(),
            "value".to_string(),
            "raw".to_string(),
        ];
        let rows = vec![vec![
            "2026-05-16T12:00:00Z".to_string(),
            "bed/1".to_string(),
            "42.0".to_string(),
            "2400".to_string(),
        ]];
        let schema = detect_schema(&headers, &rows);
        assert_eq!(schema.timestamp_idx(), Some(0));
        assert_eq!(schema.source_idx(), Some(1));
        assert_eq!(schema.raw_idx(), Some(3));
        let vc = schema.value_columns();
        assert_eq!(vc.len(), 1);
        assert_eq!(vc[0], (2, "value".to_string()));
    }

    #[test]
    fn multiple_value_columns_detected() {
        let headers = vec![
            "timestamp".to_string(),
            "temperature".to_string(),
            "humidity".to_string(),
        ];
        let rows = vec![vec![
            "2026-05-16T12:00:00Z".to_string(),
            "72.0".to_string(),
            "55.0".to_string(),
        ]];
        let schema = detect_schema(&headers, &rows);
        let vc = schema.value_columns();
        assert_eq!(vc.len(), 2);
        assert_eq!(vc[0].1, "temperature");
        assert_eq!(vc[1].1, "humidity");
    }

    #[test]
    fn parse_timestamp_rfc3339() {
        let dt = parse_timestamp("2026-05-16T12:00:00Z").unwrap();
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-05-16 12:00:00");
    }

    #[test]
    fn parse_timestamp_iso_space() {
        let dt = parse_timestamp("2026-05-16 12:00:00").unwrap();
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-05-16 12:00:00");
    }

    #[test]
    fn parse_timestamp_unix_seconds() {
        let dt = parse_timestamp("1779278400").unwrap();
        assert_eq!(dt.timestamp(), 1779278400);
    }

    #[test]
    fn parse_timestamp_unix_millis() {
        let dt = parse_timestamp("1779278400123").unwrap();
        assert_eq!(dt.timestamp_millis(), 1779278400123);
    }

    #[test]
    fn parse_timestamp_rejects_garbage() {
        assert!(parse_timestamp("not-a-date").is_err());
    }

    #[test]
    fn synthetic_timestamps_advance_by_cadence() {
        let start = Utc::now();
        let t0 = synthetic_timestamp(start, 0, Duration::seconds(30));
        let t1 = synthetic_timestamp(start, 1, Duration::seconds(30));
        let t2 = synthetic_timestamp(start, 2, Duration::seconds(30));
        assert_eq!(t0, start);
        assert_eq!((t1 - t0).num_seconds(), 30);
        assert_eq!((t2 - t1).num_seconds(), 30);
    }

    #[test]
    fn read_csv_basic() {
        let data = "timestamp,value\n2026-05-16T12:00:00Z,42.0\n2026-05-16T12:00:30Z,43.0\n";
        let (headers, rows) = read_csv(Cursor::new(data)).unwrap();
        assert_eq!(headers, vec!["timestamp", "value"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][1], "43.0");
    }

    #[test]
    fn read_csv_strips_bom() {
        let data = "\u{feff}timestamp,value\n2026-05-16T12:00:00Z,42.0\n";
        let (headers, _) = read_csv(Cursor::new(data)).unwrap();
        assert_eq!(headers[0], "timestamp");
    }

    #[test]
    fn build_readings_uses_synthetic_when_no_timestamp() {
        let headers = vec!["value".to_string()];
        let rows = vec![vec!["1.0".to_string()], vec!["2.0".to_string()]];
        let schema = detect_schema(&headers, &rows);
        let start = Utc::now();
        let (readings, skipped) = build_readings(
            &headers,
            &rows,
            &schema,
            0,
            "value",
            "test",
            start,
            Duration::seconds(30),
        )
        .unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(readings.len(), 2);
        assert_eq!((readings[1].timestamp - readings[0].timestamp).num_seconds(), 30);
    }

    #[test]
    fn build_readings_skips_blank_and_non_numeric() {
        let headers = vec!["value".to_string()];
        let rows = vec![
            vec!["1.0".to_string()],
            vec!["".to_string()],
            vec!["oops".to_string()],
            vec!["2.0".to_string()],
        ];
        let schema = detect_schema(&headers, &rows);
        let (readings, skipped) = build_readings(
            &headers,
            &rows,
            &schema,
            0,
            "value",
            "test",
            Utc::now(),
            Duration::seconds(30),
        )
        .unwrap();
        assert_eq!(readings.len(), 2);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn build_readings_uses_source_column() {
        let headers = vec![
            "timestamp".to_string(),
            "sensor".to_string(),
            "value".to_string(),
        ];
        let rows = vec![
            vec![
                "2026-05-16T12:00:00Z".to_string(),
                "bed/A".to_string(),
                "10.0".to_string(),
            ],
            vec![
                "2026-05-16T12:00:30Z".to_string(),
                "bed/B".to_string(),
                "20.0".to_string(),
            ],
        ];
        let schema = detect_schema(&headers, &rows);
        let (readings, _) = build_readings(
            &headers,
            &rows,
            &schema,
            2,
            "value",
            "default",
            Utc::now(),
            Duration::seconds(30),
        )
        .unwrap();
        assert_eq!(readings[0].source, "bed/A");
        assert_eq!(readings[1].source, "bed/B");
    }

    #[test]
    fn build_readings_sorts_by_timestamp() {
        let headers = vec!["timestamp".to_string(), "value".to_string()];
        let rows = vec![
            vec!["2026-05-16T12:00:30Z".to_string(), "2.0".to_string()],
            vec!["2026-05-16T12:00:00Z".to_string(), "1.0".to_string()],
        ];
        let schema = detect_schema(&headers, &rows);
        let (readings, _) = build_readings(
            &headers,
            &rows,
            &schema,
            1,
            "value",
            "default",
            Utc::now(),
            Duration::seconds(30),
        )
        .unwrap();
        assert_eq!(readings[0].value, 1.0);
        assert_eq!(readings[1].value, 2.0);
    }
}
