//! Render a [`Report`] in one of the supported output formats.

use crate::report::Report;
use anyhow::Result;
use colored::{ColoredString, Colorize};

const TOTAL_WIDTH: usize = 56;

/// Print a list of reports as one summary block per metric.
pub fn print_summary(reports: &[Report], color: bool, full: bool) {
    for (i, r) in reports.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_summary_block(r, color, full);
    }
}

fn print_summary_block(r: &Report, color: bool, full: bool) {
    let title = if r.metric.is_empty() {
        "gt-validate report".to_string()
    } else {
        format!("gt-validate report: {}", r.metric)
    };
    println!("{}", section_header(&title));

    let sources = if r.sources.is_empty() {
        "—".to_string()
    } else {
        r.sources.join(", ")
    };

    let duration = match (r.first_timestamp, r.last_timestamp) {
        (Some(a), Some(b)) => format!(
            "{} ({} \u{2192} {})",
            humanize_duration(r.duration_seconds),
            a.format("%Y-%m-%d %H:%M"),
            b.format("%H:%M"),
        ),
        _ => "—".to_string(),
    };

    println!("{:<11} {}", "File:", r.file);
    println!("{:<11} {}", "Readings:", format_int(r.total_readings));
    println!("{:<11} {}", "Metric:", r.metric);
    println!("{:<11} {}", "Source(s):", sources);
    println!("{:<11} {}", "Duration:", duration);
    if r.skipped_rows > 0 {
        println!(
            "{:<11} {} non-numeric/empty rows",
            "Skipped:", r.skipped_rows
        );
    }

    println!("{}", section_header("Quality breakdown"));
    let q = &r.quality;
    let good_label = colored_label("Good:", "green", color);
    let suspect_label = colored_label("Suspect:", "yellow", color);
    let invalid_label = colored_label("Invalid:", "red", color);
    println!(
        "{}    {}  ({})  {}",
        good_label,
        format_int_width(q.good, 8),
        format_pct(q.good_percent),
        bar(q.good_percent),
    );
    println!(
        "{} {}  ({})  {}",
        suspect_label,
        format_int_width(q.suspect, 8),
        format_pct(q.suspect_percent),
        bar(q.suspect_percent),
    );
    println!(
        "{} {}  ({})  {}",
        invalid_label,
        format_int_width(q.invalid, 8),
        format_pct(q.invalid_percent),
        bar(q.invalid_percent),
    );

    if !r.flagged_rules.is_empty() {
        println!("{}", section_header("Flagged readings"));
        for (rule, count) in &r.flagged_rules {
            println!("{:<20} {:>3}  readings", format!("{}:", rule), count);
        }
    }

    println!("{}", section_header("Health score"));
    let badge = status_badge(r.health.status, color);
    println!(
        "Overall:           {:>5.1} / 100  [{}]",
        r.health.overall, badge
    );
    println!(
        "Quality rate:      {:>5.1}%  (weight: 35%)",
        r.health.quality_rate * 100.0
    );
    println!(
        "Reporting cadence: {:>5.1}%  (weight: 25%)",
        r.health.reporting_cadence * 100.0
    );
    println!(
        "Drift:             {:>5.1}%  (weight: 20%)",
        r.health.drift * 100.0
    );
    println!(
        "Variance ratio:    {:>5.1}%  (weight: 10%)",
        r.health.variance_ratio * 100.0
    );
    println!(
        "Recency:           {:>5.1}%  (weight: 10%)",
        r.health.recency * 100.0
    );

    println!("{}", section_header("Quarantine"));
    let q_status = match r.quarantine.status {
        "quarantined" => maybe_color("Quarantined", "red", color),
        _ => maybe_color("Active", "green", color),
    };
    println!("Status: {}", q_status);
    println!("Quarantine events: {}", r.quarantine.events);

    if full && !r.flagged_readings.is_empty() {
        println!("{}", section_header("Flagged readings (detail)"));
        for fr in &r.flagged_readings {
            let q_label = match fr.quality.as_str() {
                "suspect" => maybe_color("SUSPECT", "yellow", color),
                "invalid" => maybe_color("INVALID", "red", color),
                other => maybe_color(other, "white", color),
            };
            println!(
                "{}  value={:<8}  {}  {}: {}",
                fr.timestamp.format("%Y-%m-%d %H:%M:%S"),
                format!("{:.3}", fr.value),
                q_label,
                fr.rule,
                fr.reason,
            );
        }
    }

    println!("{}", "─".repeat(TOTAL_WIDTH));
}

pub fn print_json(reports: &[Report]) -> Result<()> {
    let out = if reports.len() == 1 {
        serde_json::to_string_pretty(&reports[0])?
    } else {
        serde_json::to_string_pretty(reports)?
    };
    println!("{}", out);
    Ok(())
}

fn section_header(title: &str) -> String {
    let prefix_len = 3 + title.chars().count() + 1; // "── " + title + " "
    let fill = TOTAL_WIDTH.saturating_sub(prefix_len);
    format!("\u{2500}\u{2500} {} {}", title, "\u{2500}".repeat(fill))
}

const BAR_WIDTH: usize = 21;

fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let unfilled = BAR_WIDTH - filled;
    let mut s = String::with_capacity(BAR_WIDTH * 3);
    for _ in 0..filled {
        s.push('\u{2588}');
    }
    for _ in 0..unfilled.max(1) {
        s.push('\u{2591}');
    }
    s
}

fn format_int(n: u64) -> String {
    // Simple thousands-separator using commas.
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

fn format_int_width(n: u64, width: usize) -> String {
    format!("{:>width$}", format_int(n), width = width)
}

fn format_pct(p: f64) -> String {
    format!("{:>4.1}%", p)
}

fn humanize_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "0s".into();
    }
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{}h {}m", h, m)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}

fn colored_label(label: &str, color: &str, enable: bool) -> ColoredString {
    maybe_color(label, color, enable)
}

fn maybe_color(s: &str, color: &str, enable: bool) -> ColoredString {
    if !enable {
        return s.normal();
    }
    match color {
        "green" => s.green(),
        "yellow" => s.yellow(),
        "red" => s.red(),
        _ => s.normal(),
    }
}

fn status_badge(status: &str, color: bool) -> ColoredString {
    let label = match status {
        "healthy" => "Healthy",
        "degraded" => "Degraded",
        "suspect" => "Suspect",
        "bad" => "Bad",
        _ => "Unknown",
    };
    let c = match status {
        "healthy" => "green",
        "degraded" | "suspect" => "yellow",
        "bad" => "red",
        _ => "white",
    };
    maybe_color(label, c, color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{HealthSummary, QualityCounts, QuarantineSummary, ReportBuilder};
    use chrono::{Duration, Utc};
    use groundtruth_validator::QualityLevel;

    fn make_report() -> Report {
        let mut b = ReportBuilder::new("test.csv", "value");
        let t = Utc::now();
        for i in 0..10i64 {
            b.record(
                "src",
                t + Duration::seconds(30 * i),
                40.0 + i as f64,
                QualityLevel::Good,
                "",
                "",
            );
        }
        b.record(
            "src",
            t + Duration::seconds(300),
            999.0,
            QualityLevel::Invalid,
            "value_range",
            "outside",
        );
        let hs = HealthSummary {
            overall: 87.4,
            status: "degraded",
            quality_rate: 0.961,
            reporting_cadence: 0.982,
            drift: 0.92,
            variance_ratio: 0.885,
            recency: 1.0,
            readings_in_window: 11,
        };
        let qs = QuarantineSummary {
            status: "active",
            events: 0,
        };
        b.finalize(hs, qs)
    }

    #[test]
    fn json_output_has_required_fields() {
        let r = make_report();
        let s = serde_json::to_string(&r).unwrap();
        for field in [
            "\"file\":",
            "\"metric\":",
            "\"sources\":",
            "\"total_readings\":",
            "\"duration_seconds\":",
            "\"quality\":",
            "\"flagged_rules\":",
            "\"flagged_readings\":",
            "\"health\":",
            "\"quarantine\":",
        ] {
            assert!(s.contains(field), "missing field {} in {}", field, s);
        }
    }

    #[test]
    fn json_output_is_valid_json() {
        let r = make_report();
        let s = serde_json::to_string(&r).unwrap();
        let _: serde_json::Value = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn bar_clamps_to_width() {
        let full = bar(100.0);
        assert_eq!(full.chars().filter(|c| *c == '\u{2588}').count(), BAR_WIDTH);
    }

    #[test]
    fn bar_empty_still_shows_track() {
        let empty = bar(0.0);
        assert!(empty.contains('\u{2591}'));
        assert_eq!(empty.chars().filter(|c| *c == '\u{2588}').count(), 0);
    }

    #[test]
    fn section_header_has_expected_width() {
        let h = section_header("Quality breakdown");
        // Width is measured in chars, not bytes.
        assert_eq!(h.chars().count(), TOTAL_WIDTH);
    }

    #[test]
    fn humanize_basics() {
        assert_eq!(humanize_duration(0), "0s");
        assert_eq!(humanize_duration(45), "45s");
        assert_eq!(humanize_duration(125), "2m 5s");
        assert_eq!(humanize_duration(3725), "1h 2m");
    }

    #[test]
    fn quality_counts_finalize_zero() {
        let mut q = QualityCounts::default();
        q.finalize();
        assert_eq!(q.good_percent, 0.0);
    }
}
