//! Orchestrates: parse args → read CSV → detect schema → build
//! [`StreamValidator`] → push readings → produce [`Report`]s.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use groundtruth_validator::{MetricConfig, QuarantineTransition, StreamValidator, ValidatorConfig};
use std::io::Read;

use crate::args::Args;
use crate::csv_input::{build_readings, detect_schema, open_reader, read_csv};
use crate::report::{HealthSummary, QuarantineSummary, Report, ReportBuilder};

/// Build the validator config for one metric column.
///
/// Stuck detection is per-metric, so the auto-detected (or
/// `--metric`-supplied) column name selects the same profile the
/// server uses: `temperature` and `humidity` get the time-based rule
/// at 0.1 resolution over a 60-minute window, everything else keeps
/// the count-based rule. `--stuck-mode`, `--resolution`, and
/// `--stuck-window` override that per invocation.
pub fn build_config_for(metric: &str, args: &Args) -> ValidatorConfig {
    let mut mc = MetricConfig::new(
        args.range
            .clone()
            .unwrap_or(f64::NEG_INFINITY..=f64::INFINITY),
    )
    .with_max_rate_of_change(args.max_rate)
    .with_rate_window(args.cadence * 10)
    .with_stuck_count(args.stuck_count)
    .with_expected_cadence(args.cadence)
    .with_stuck_defaults_for(metric);
    if let Some(rr) = args.raw_range.clone() {
        mc = mc.with_raw_range(rr);
    }
    if let Some(mode) = args.stuck_mode {
        mc = mc.with_stuck_mode(mode.into());
    }
    if let Some(resolution) = args.resolution {
        mc = mc.with_resolution(resolution);
    }
    if let Some(window) = args.stuck_window {
        mc = mc.with_stuck_window(window);
    }
    ValidatorConfig::builder().metric(metric, mc).build()
}

/// Top-level entry point used by both `main` and integration tests.
/// Returns the produced reports — one per metric processed.
pub fn run(args: &Args) -> Result<Vec<Report>> {
    let reader = open_reader(&args.file)?;
    run_with_reader(args, reader)
}

pub fn run_with_reader<R: Read>(args: &Args, reader: R) -> Result<Vec<Report>> {
    let (headers, rows) = read_csv(reader)?;
    if rows.is_empty() {
        bail!("CSV had no data rows");
    }
    let schema = detect_schema(&headers, &rows);

    // Select which metric columns to process. If --metric is given, we
    // expect a matching header (or fall back to the first numeric).
    let value_columns = schema.value_columns();
    if value_columns.is_empty() {
        bail!("no numeric value columns detected in CSV");
    }

    let selected: Vec<(usize, String)> = match &args.metric {
        Some(name) => {
            // Prefer an exact header match.
            if let Some(found) = value_columns
                .iter()
                .find(|(_, m)| m.eq_ignore_ascii_case(name))
            {
                vec![found.clone()]
            } else {
                // Fall back to the first numeric column, but tag it
                // with the user-specified metric name so the report
                // matches what they asked for.
                let (idx, _) = value_columns[0].clone();
                vec![(idx, name.clone())]
            }
        }
        None => value_columns.clone(),
    };

    let synthetic_start = Utc::now();
    let mut reports = Vec::with_capacity(selected.len());

    for (col_idx, metric_name) in selected {
        let mut validator = StreamValidator::new(build_config_for(&metric_name, args));
        let mut builder = ReportBuilder::new(args.file.clone(), metric_name.clone());

        let (readings, skipped) = build_readings(
            &headers,
            &rows,
            &schema,
            col_idx,
            &metric_name,
            &args.source,
            synthetic_start,
            args.cadence,
        )
        .with_context(|| format!("failed to parse readings for metric '{}'", metric_name))?;

        builder.add_skipped(skipped);

        for r in &readings {
            let res = validator.validate(r.clone());
            builder.record(
                &r.source,
                r.timestamp,
                r.value,
                res.quality,
                &res.rule,
                &res.reason,
            );
        }

        // Anchor Tier-2 to the data's last timestamp so historical
        // CSVs produce a meaningful health score instead of one that's
        // crushed by wall-clock recency.
        let anchor = readings
            .last()
            .map(|r| r.timestamp)
            .unwrap_or_else(Utc::now);
        let scores = validator.update_health_at(anchor);
        let transitions = validator.update_quarantine_at(anchor);
        let quarantine_events = transitions
            .iter()
            .filter(|(_, _, t)| *t == QuarantineTransition::Entered)
            .count() as u64;
        // For the headline, summarize the worst (lowest-score) source.
        let worst_score = scores
            .iter()
            .map(|(_, _, s)| s)
            .min_by(|a, b| {
                a.overall
                    .partial_cmp(&b.overall)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or_else(|| groundtruth_validator::HealthScore {
                overall: 0.0,
                quality_rate: 0.0,
                reporting_cadence: 0.0,
                drift: 0.0,
                variance_ratio: 0.0,
                recency: 0.0,
                readings_in_window: 0,
                last_good_reading_at: None,
                computed_at: Utc::now(),
            });
        let health = HealthSummary::from_score(&worst_score);

        let any_quarantined = scores.iter().any(|(src, metric, _)| {
            validator
                .quarantine_status(src, metric)
                .map(|s| s.is_quarantined)
                .unwrap_or(false)
        });
        let quarantine = QuarantineSummary {
            status: if any_quarantined {
                "quarantined"
            } else {
                "active"
            },
            events: quarantine_events,
        };

        reports.push(builder.finalize(health, quarantine));
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::OutputFormat;
    use chrono::Duration;
    use groundtruth_validator::StuckMode;
    use std::io::Cursor;
    use std::ops::RangeInclusive;

    fn base_args() -> Args {
        Args {
            file: "<inline>".into(),
            metric: None,
            source: "default".into(),
            range: None,
            raw_range: None,
            cadence: Duration::seconds(30),
            stuck_count: 6,
            stuck_mode: None,
            resolution: None,
            stuck_window: None,
            max_rate: 30.0,
            format: OutputFormat::Summary,
            no_color: true,
        }
    }

    #[test]
    fn run_inline_csv_clean() {
        let mut data = String::from("timestamp,value\n");
        for i in 0..20 {
            data.push_str(&format!(
                "2026-05-16T12:00:{:02}Z,{}\n",
                i * 3,
                40.0 + i as f64 * 0.5
            ));
        }
        let args = base_args();
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(r.total_readings, 20);
        assert_eq!(r.quality.invalid, 0);
        assert_eq!(r.quality.suspect, 0);
    }

    #[test]
    fn run_inline_csv_with_stuck() {
        let mut data = String::from("value\n");
        for _ in 0..10 {
            data.push_str("42.5\n");
        }
        let args = base_args();
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        let r = &reports[0];
        // After 5 prior readings of 42.5, the 6th and beyond trip stuck.
        assert!(r.quality.suspect > 0, "stuck rule should have fired");
        assert!(r.flagged_rules.contains_key("stuck_reading"));
    }

    #[test]
    fn run_with_range_makes_out_of_band_invalid() {
        let data = "value\n10.0\n10.5\n999.0\n10.6\n";
        let mut args = base_args();
        args.range = Some::<RangeInclusive<f64>>(0.0..=100.0);
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        let r = &reports[0];
        assert_eq!(r.quality.invalid, 1);
        assert!(r.flagged_rules.contains_key("value_range"));
    }

    #[test]
    fn run_multi_metric_processes_each_column() {
        let data = "timestamp,temperature,humidity\n\
                    2026-05-16T12:00:00Z,72.0,55.0\n\
                    2026-05-16T12:00:30Z,72.5,56.0\n\
                    2026-05-16T12:01:00Z,73.0,57.0\n";
        let args = base_args();
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        assert_eq!(reports.len(), 2);
        let metrics: Vec<_> = reports.iter().map(|r| r.metric.clone()).collect();
        assert!(metrics.contains(&"temperature".to_string()));
        assert!(metrics.contains(&"humidity".to_string()));
    }

    #[test]
    fn run_metric_override_renames_first_column() {
        let data = "value\n1.0\n2.0\n3.0\n";
        let mut args = base_args();
        args.metric = Some("cpu_pct".into());
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].metric, "cpu_pct");
    }

    #[test]
    fn run_empty_csv_errors() {
        let data = "value\n";
        let args = base_args();
        let err = run_with_reader(&args, Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("no data rows"));
    }

    #[test]
    fn run_no_numeric_columns_errors() {
        let data = "name,note\nfoo,hello\nbar,world\n";
        let args = base_args();
        let err = run_with_reader(&args, Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("no numeric value columns"));
    }

    // ---- Per-metric stuck detection --------------------------------

    /// 90 minutes of a DHT22 at 30s cadence, ramping 0.1 °F every
    /// 5 minutes — a healthy warm-up under one header name.
    fn ramp_csv(column: &str) -> String {
        let mut data = format!("timestamp,{}\n", column);
        for i in 0..180i64 {
            let secs = i * 30;
            let ts = chrono::DateTime::parse_from_rfc3339("2026-05-16T12:00:00Z")
                .unwrap()
                .to_utc()
                + Duration::seconds(secs);
            data.push_str(&format!(
                "{},{:.1}\n",
                ts.to_rfc3339(),
                65.8 + (secs / 300) as f64 * 0.1
            ));
        }
        data
    }

    #[test]
    fn detected_temperature_column_uses_time_based_stuck() {
        let cfg = build_config_for("temperature", &base_args());
        let mc = cfg.metric("temperature").unwrap();
        assert_eq!(mc.stuck_mode, StuckMode::Duration);
        assert_eq!(mc.resolution, Some(0.1));
        assert_eq!(mc.stuck_window, Duration::minutes(60));
    }

    #[test]
    fn detected_moisture_column_keeps_count_based_stuck() {
        let cfg = build_config_for("moisture", &base_args());
        let mc = cfg.metric("moisture").unwrap();
        assert_eq!(mc.stuck_mode, StuckMode::Count);
        assert_eq!(mc.stuck_count, 6);
    }

    #[test]
    fn flags_explicitly_override_the_per_metric_defaults() {
        let mut args = base_args();
        args.stuck_mode = Some(crate::args::StuckModeArg::Count);
        let cfg = build_config_for("temperature", &args);
        assert_eq!(
            cfg.metric("temperature").unwrap().stuck_mode,
            StuckMode::Count
        );

        let mut args = base_args();
        args.stuck_mode = Some(crate::args::StuckModeArg::Duration);
        args.resolution = Some(25.0);
        args.stuck_window = Some(Duration::minutes(15));
        let cfg = build_config_for("raw_adc", &args);
        let mc = cfg.metric("raw_adc").unwrap();
        assert_eq!(mc.stuck_mode, StuckMode::Duration);
        assert_eq!(mc.resolution, Some(25.0));
        assert_eq!(mc.stuck_window, Duration::minutes(15));
    }

    #[test]
    fn temperature_ramp_is_clean_but_the_same_data_as_moisture_is_not() {
        let args = base_args();
        let temp = run_with_reader(&args, Cursor::new(ramp_csv("temperature"))).unwrap();
        assert_eq!(temp[0].quality.suspect, 0, "healthy DHT22 warm-up");

        let moisture = run_with_reader(&args, Cursor::new(ramp_csv("moisture"))).unwrap();
        assert!(
            moisture[0].quality.suspect > 0,
            "count-based rule is still in force for moisture"
        );
    }

    #[test]
    fn run_synthetic_timestamps_when_missing() {
        let data = "value\n10.0\n11.0\n12.0\n";
        let args = base_args();
        let reports = run_with_reader(&args, Cursor::new(data)).unwrap();
        let r = &reports[0];
        // 3 readings × 30s cadence = 60s span.
        assert_eq!(r.duration_seconds, 60);
    }
}
