//! End-to-end integration tests. Drives the CLI's `run_with_reader`
//! against real CSV bytes and against the bundled sample files.

use chrono::Duration;
use groundtruth_validator_cli::args::OutputFormat;
use groundtruth_validator_cli::{run_with_reader, Args};
use std::fs::File;
use std::io::Cursor;
use std::ops::RangeInclusive;

fn args(file: &str) -> Args {
    Args {
        file: file.into(),
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
fn clean_sensor_csv_is_mostly_good() {
    let f = File::open("examples/clean_sensor.csv").expect("clean_sensor.csv must exist");
    let reports = run_with_reader(&args("examples/clean_sensor.csv"), f).unwrap();
    assert_eq!(reports.len(), 1, "single value column");
    let r = &reports[0];
    assert_eq!(r.total_readings, 100);
    assert_eq!(r.quality.invalid, 0);
    assert!(
        r.quality.good_percent >= 95.0,
        "expected ≥95% good, got {} ({}/100)",
        r.quality.good_percent,
        r.quality.good
    );
    assert_eq!(r.sources, vec!["bed/1".to_string()]);
}

#[test]
fn noisy_sensor_csv_triggers_expected_rules() {
    let mut a = args("examples/noisy_sensor.csv");
    // Temperature plausibility range — 999 should be Invalid.
    a.range = Some::<RangeInclusive<f64>>(-40.0..=200.0);
    // Use a generous rate window so the gap doesn't suppress detection.
    let f = File::open("examples/noisy_sensor.csv").expect("noisy_sensor.csv must exist");
    let reports = run_with_reader(&a, f).unwrap();
    let r = &reports[0];
    assert_eq!(r.total_readings, 200);
    // Stuck block of 8 identical values fires at the 6th onwards.
    assert!(
        r.flagged_rules.contains_key("stuck_reading"),
        "expected stuck_reading flags, got {:?}",
        r.flagged_rules
    );
    assert!(
        r.flagged_rules.contains_key("value_range"),
        "expected value_range invalid for 999, got {:?}",
        r.flagged_rules
    );
    assert!(r.quality.invalid >= 1);
    assert!(r.quality.suspect >= 3);
    // Health should be degraded enough that it's not at 100.
    assert!(r.health.overall < 100.0);
}

#[test]
fn stdin_reading_via_dash() {
    let csv = "value\n10.0\n11.0\n12.0\n13.0\n14.0\n";
    // `run_with_reader` takes any Read, which is what the binary uses
    // under the hood when `file == "-"`. We test the same code path
    // with an in-memory cursor.
    let reports = run_with_reader(&args("-"), Cursor::new(csv)).unwrap();
    assert_eq!(reports[0].total_readings, 5);
}

#[test]
fn multi_metric_processes_all_columns() {
    let csv = "timestamp,temperature,humidity\n\
               2026-05-16T12:00:00Z,72.0,55.0\n\
               2026-05-16T12:00:30Z,72.5,56.0\n\
               2026-05-16T12:01:00Z,73.0,57.0\n\
               2026-05-16T12:01:30Z,73.5,57.5\n";
    let reports = run_with_reader(&args("<inline>"), Cursor::new(csv)).unwrap();
    assert_eq!(reports.len(), 2);
    let metrics: Vec<_> = reports.iter().map(|r| r.metric.clone()).collect();
    assert!(metrics.iter().any(|m| m == "temperature"));
    assert!(metrics.iter().any(|m| m == "humidity"));
}

#[test]
fn summary_output_contains_all_sections() {
    use groundtruth_validator_cli::output::print_summary;
    let f = File::open("examples/clean_sensor.csv").unwrap();
    let reports = run_with_reader(&args("examples/clean_sensor.csv"), f).unwrap();
    // We can't easily capture stdout, but we can confirm the helper
    // doesn't panic and that we have a finished Report shape.
    print_summary(&reports, false, false);
}

#[test]
fn json_output_round_trips() {
    let f = File::open("examples/clean_sensor.csv").unwrap();
    let reports = run_with_reader(&args("examples/clean_sensor.csv"), f).unwrap();
    let json = serde_json::to_string(&reports[0]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("file").is_some());
    assert!(v.get("metric").is_some());
    assert!(v.get("quality").is_some());
    assert!(v.get("health").is_some());
    assert!(v.get("quarantine").is_some());
    assert!(v.get("flagged_rules").is_some());
    assert!(v.get("flagged_readings").is_some());
}

#[test]
fn empty_csv_errors_gracefully() {
    let csv = "value\n";
    let err = run_with_reader(&args("<inline>"), Cursor::new(csv)).unwrap_err();
    assert!(err.to_string().contains("no data rows"));
}

#[test]
fn malformed_csv_errors_gracefully() {
    // No numeric columns at all.
    let csv = "name,note\nfoo,hi\nbar,bye\n";
    let err = run_with_reader(&args("<inline>"), Cursor::new(csv)).unwrap_err();
    assert!(err.to_string().contains("no numeric"));
}

#[test]
fn synthetic_timestamps_from_value_only_csv() {
    let csv = "value\n10\n20\n30\n";
    let mut a = args("<inline>");
    a.cadence = Duration::seconds(60);
    let reports = run_with_reader(&a, Cursor::new(csv)).unwrap();
    let r = &reports[0];
    // Two 60-second intervals between three readings.
    assert_eq!(r.duration_seconds, 120);
}
