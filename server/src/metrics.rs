//! Prometheus instrumentation.
//!
//! Beyond exporting values, this module owns **stale-stream handling**.
//! A Prometheus gauge holds its last value forever, so a sensor that
//! dies mid-afternoon keeps getting scraped at whatever it last said
//! and Grafana draws a flat line — a dead sensor and a stable one look
//! identical. Instead, we track when each stream last reported and
//! *stop exporting its value gauge* once it has been silent past
//! [`stale_timeout`]. Prometheus then records nothing for that series
//! and Grafana breaks the line, which is the honest picture.
//!
//! Health and quarantine gauges are deliberately exempt: when a sensor
//! dies, those are the series you want to keep watching.
//!
//! Time is passed in rather than read from the clock ([`record_reading_at`],
//! [`refresh_stale_gauges`]) so the behavior is testable without sleeping.

use chrono::{DateTime, Duration, Utc};
use once_cell::sync::Lazy;
use prometheus::{Encoder, GaugeVec, IntCounterVec, Registry, TextEncoder};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// Default silence after which a stream's value gauge stops being
/// exported. Long enough to survive a missed publish at the 60s
/// cadence, short enough that a real outage shows up promptly.
pub const DEFAULT_STALE_TIMEOUT_SECS: i64 = 300;

static STALE_TIMEOUT_SECS: AtomicI64 = AtomicI64::new(DEFAULT_STALE_TIMEOUT_SECS);

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static MOISTURE_PCT: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_moisture_percent",
            "Calibrated soil moisture percentage (0-100)",
        ),
        &["zone", "zone_id"],
    )
    .expect("moisture gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register moisture");
    g
});

pub static MOISTURE_RAW: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_moisture_raw_adc",
            "Raw ADC reading from capacitive soil sensor (0-4095)",
        ),
        &["zone", "zone_id"],
    )
    .expect("moisture_raw gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register moisture_raw");
    g
});

pub static TEMPERATURE_F: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_temperature_fahrenheit",
            "Temperature in Fahrenheit",
        ),
        &["zone", "zone_id"],
    )
    .expect("temperature gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register temperature");
    g
});

pub static HUMIDITY_PCT: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_humidity_percent",
            "Relative humidity percentage (0-100)",
        ),
        &["zone", "zone_id"],
    )
    .expect("humidity gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register humidity");
    g
});

pub static SENSOR_HEALTH_SCORE: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_sensor_health_score",
            "Sensor health score 0-100, computed from quality rate, cadence, drift, variance, and recency",
        ),
        &["zone", "zone_id", "metric"],
    )
    .expect("sensor_health_score gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register sensor_health_score");
    g
});

pub static SENSOR_QUARANTINED: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_sensor_quarantined",
            "1.0 if the sensor is currently quarantined (Tier-2 health below 40 for 3+ consecutive checks), 0.0 otherwise",
        ),
        &["zone", "zone_id", "metric"],
    )
    .expect("sensor_quarantined gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register sensor_quarantined");
    g
});

pub static QUARANTINE_EVENTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new(
            "groundtruth_quarantine_events_total",
            "Total times a sensor has entered quarantine",
        ),
        &["zone", "zone_id", "metric"],
    )
    .expect("quarantine_events counter");
    REGISTRY
        .register(Box::new(c.clone()))
        .expect("register quarantine_events");
    c
});

pub static LAST_READING_AGE: Lazy<GaugeVec> = Lazy::new(|| {
    let g = GaugeVec::new(
        prometheus::Opts::new(
            "groundtruth_last_reading_age_seconds",
            "Seconds since the last reading was received for this stream. Keeps climbing while a stream is silent; the stream's value gauge stops being exported once this passes the stale timeout",
        ),
        &["zone", "zone_id", "metric"],
    )
    .expect("last_reading_age gauge");
    REGISTRY
        .register(Box::new(g.clone()))
        .expect("register last_reading_age");
    g
});

pub static READINGS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new(
            "groundtruth_readings_total",
            "Total number of readings ingested, partitioned by validation quality",
        ),
        &["zone", "zone_id", "metric", "quality"],
    )
    .expect("readings counter");
    REGISTRY
        .register(Box::new(c.clone()))
        .expect("register readings");
    c
});

/// A `(zone, zone_id, metric)` stream we've seen at least one reading
/// for.
type StreamKey = (String, String, String);

struct StreamState {
    last_reading_at: DateTime<Utc>,
    /// Whether this stream's value gauge is currently exported. False
    /// after we've removed it for staleness, so `refresh_stale_gauges`
    /// doesn't try to remove it again on every tick.
    value_exported: bool,
}

static STREAMS: Lazy<Mutex<HashMap<StreamKey, StreamState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// How long a stream may be silent before its value gauge stops being
/// exported.
pub fn stale_timeout() -> Duration {
    Duration::seconds(STALE_TIMEOUT_SECS.load(Ordering::Relaxed))
}

/// Override the stale timeout. Called once at startup from
/// `STALE_TIMEOUT_SECS`; non-positive values are ignored.
pub fn set_stale_timeout_secs(secs: i64) {
    if secs > 0 {
        STALE_TIMEOUT_SECS.store(secs, Ordering::Relaxed);
    }
}

pub fn record_reading(
    zone: &str,
    zone_id: &str,
    metric: &str,
    value: f64,
    raw_adc: Option<i64>,
    quality: &str,
) {
    record_reading_at(Utc::now(), zone, zone_id, metric, value, raw_adc, quality)
}

/// Like [`record_reading`], with the arrival time supplied by the
/// caller. Receiving *any* reading — good, suspect, or invalid — marks
/// the stream alive; only Good readings update the value gauges.
#[allow(clippy::too_many_arguments)]
pub fn record_reading_at(
    now: DateTime<Utc>,
    zone: &str,
    zone_id: &str,
    metric: &str,
    value: f64,
    raw_adc: Option<i64>,
    quality: &str,
) {
    READINGS_TOTAL
        .with_label_values(&[zone, zone_id, metric, quality])
        .inc();

    let mut exported_value = false;
    if quality == "good" {
        match metric {
            "moisture" => {
                MOISTURE_PCT.with_label_values(&[zone, zone_id]).set(value);
                if let Some(raw) = raw_adc {
                    MOISTURE_RAW
                        .with_label_values(&[zone, zone_id])
                        .set(raw as f64);
                }
                exported_value = true;
            }
            "temperature" => {
                TEMPERATURE_F.with_label_values(&[zone, zone_id]).set(value);
                exported_value = true;
            }
            "humidity" => {
                HUMIDITY_PCT.with_label_values(&[zone, zone_id]).set(value);
                exported_value = true;
            }
            _ => {}
        }
    }

    LAST_READING_AGE
        .with_label_values(&[zone, zone_id, metric])
        .set(0.0);

    if let Ok(mut streams) = STREAMS.lock() {
        let entry = streams
            .entry((zone.to_string(), zone_id.to_string(), metric.to_string()))
            .or_insert(StreamState {
                last_reading_at: now,
                value_exported: false,
            });
        entry.last_reading_at = now;
        // `with_label_values` above re-created the series, so a stream
        // coming back from silence is exported again from this reading.
        entry.value_exported |= exported_value;
    }
}

/// Refresh `groundtruth_last_reading_age_seconds` for every known
/// stream and drop the value gauges of streams that have gone quiet.
/// Call on the periodic health tick so ages keep climbing while a
/// stream is silent, not only when readings arrive.
pub fn refresh_stale_gauges(now: DateTime<Utc>) {
    let timeout = stale_timeout();
    let mut streams = match STREAMS.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    for ((zone, zone_id, metric), state) in streams.iter_mut() {
        let age = now - state.last_reading_at;
        LAST_READING_AGE
            .with_label_values(&[zone, zone_id, metric])
            .set(age.num_milliseconds() as f64 / 1000.0);

        if age > timeout && state.value_exported {
            remove_value_gauges(zone, zone_id, metric);
            state.value_exported = false;
        }
    }
}

/// Delete a stream's value series from the gauge families so it stops
/// appearing in `/metrics` entirely. Prometheus records a gap rather
/// than a repeated stale sample, which is what makes Grafana break the
/// line instead of drawing a flat one.
fn remove_value_gauges(zone: &str, zone_id: &str, metric: &str) {
    let labels = [zone, zone_id];
    // Absent series are fine to "remove" — the stream may never have
    // produced a Good reading — so the errors are deliberately ignored.
    match metric {
        "moisture" => {
            let _ = MOISTURE_PCT.remove_label_values(&labels);
            let _ = MOISTURE_RAW.remove_label_values(&labels);
        }
        "temperature" => {
            let _ = TEMPERATURE_F.remove_label_values(&labels);
        }
        "humidity" => {
            let _ = HUMIDITY_PCT.remove_label_values(&labels);
        }
        _ => {}
    }
}

pub fn render() -> String {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    let metric_families = REGISTRY.gather();
    if encoder.encode(&metric_families, &mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8(buffer).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `refresh_stale_gauges` sweeps every known stream at a
    /// caller-supplied `now`, so two tests running in parallel would
    /// age each other's streams. Serialize them and start each with an
    /// empty stream table. Series left in the registry by earlier
    /// tests are harmless because every test uses its own `zone_id`.
    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut streams) = STREAMS.lock() {
            streams.clear();
        }
        guard
    }

    /// Tests share one process-wide registry, so each uses its own
    /// `zone_id` and looks only at series carrying it.
    fn series_for(zone_id: &str, name: &str) -> Option<String> {
        let needle = format!("zone_id=\"{}\"", zone_id);
        render()
            .lines()
            .find(|l| l.starts_with(name) && l.contains(&needle))
            .map(|l| l.to_string())
    }

    fn sample(zone_id: &str, name: &str) -> Option<f64> {
        series_for(zone_id, name)
            .and_then(|l| l.rsplit(' ').next().and_then(|v| v.parse().ok()))
    }

    #[test]
    fn silent_stream_loses_its_value_gauge_and_gains_age() {
        let _guard = isolated();
        let id = "stale-temp";
        let t0 = Utc::now();
        record_reading_at(t0, "bed", id, "temperature", 73.0, None, "good");

        // Freshly reported: value present, age zero.
        assert_eq!(
            sample(id, "groundtruth_temperature_fahrenheit"),
            Some(73.0)
        );
        refresh_stale_gauges(t0);
        assert_eq!(sample(id, "groundtruth_last_reading_age_seconds"), Some(0.0));

        // Silent, but inside the timeout: still exported.
        refresh_stale_gauges(t0 + Duration::seconds(120));
        assert_eq!(
            sample(id, "groundtruth_temperature_fahrenheit"),
            Some(73.0),
            "should still be exported before the 300s timeout"
        );
        assert_eq!(
            sample(id, "groundtruth_last_reading_age_seconds"),
            Some(120.0)
        );

        // Past the timeout: the value series disappears rather than
        // reporting a stale 73.0 forever.
        refresh_stale_gauges(t0 + Duration::seconds(301));
        assert_eq!(
            series_for(id, "groundtruth_temperature_fahrenheit"),
            None,
            "stale value gauge must be absent from /metrics"
        );

        // Age keeps climbing while it's gone — the sensor's silence is
        // itself a visible signal.
        refresh_stale_gauges(t0 + Duration::seconds(900));
        assert_eq!(
            sample(id, "groundtruth_last_reading_age_seconds"),
            Some(900.0)
        );

        // And a new reading restores it.
        let back = t0 + Duration::seconds(920);
        record_reading_at(back, "bed", id, "temperature", 65.8, None, "good");
        assert_eq!(
            sample(id, "groundtruth_temperature_fahrenheit"),
            Some(65.8),
            "a new reading must restore the value gauge"
        );
        refresh_stale_gauges(back);
        assert_eq!(sample(id, "groundtruth_last_reading_age_seconds"), Some(0.0));
    }

    #[test]
    fn stale_moisture_drops_both_value_and_raw_gauges() {
        let _guard = isolated();
        let id = "stale-moisture";
        let t0 = Utc::now();
        record_reading_at(t0, "bed", id, "moisture", 42.5, Some(2400), "good");
        assert!(series_for(id, "groundtruth_moisture_percent").is_some());
        assert!(series_for(id, "groundtruth_moisture_raw_adc").is_some());

        refresh_stale_gauges(t0 + Duration::seconds(301));
        assert_eq!(series_for(id, "groundtruth_moisture_percent"), None);
        assert_eq!(series_for(id, "groundtruth_moisture_raw_adc"), None);
    }

    #[test]
    fn health_and_quarantine_gauges_survive_the_silence() {
        let _guard = isolated();
        // The point of the whole feature: when a sensor dies, the
        // series that say *it died* must stay visible.
        let id = "stale-health";
        let t0 = Utc::now();
        record_reading_at(t0, "bed", id, "temperature", 73.0, None, "good");
        SENSOR_HEALTH_SCORE
            .with_label_values(&["bed", id, "temperature"])
            .set(12.0);
        SENSOR_QUARANTINED
            .with_label_values(&["bed", id, "temperature"])
            .set(1.0);

        refresh_stale_gauges(t0 + Duration::seconds(3600));

        assert_eq!(series_for(id, "groundtruth_temperature_fahrenheit"), None);
        assert_eq!(sample(id, "groundtruth_sensor_health_score"), Some(12.0));
        assert_eq!(sample(id, "groundtruth_sensor_quarantined"), Some(1.0));
    }

    #[test]
    fn non_good_readings_keep_the_stream_alive_without_exporting_a_value() {
        let _guard = isolated();
        let id = "stale-suspect";
        let t0 = Utc::now();
        record_reading_at(t0, "bed", id, "temperature", 999.0, None, "invalid");

        // No value gauge was ever exported for this stream...
        assert_eq!(series_for(id, "groundtruth_temperature_fahrenheit"), None);
        // ...but it is a known, live stream, so its age is tracked.
        refresh_stale_gauges(t0 + Duration::seconds(60));
        assert_eq!(sample(id, "groundtruth_last_reading_age_seconds"), Some(60.0));
    }

    #[test]
    fn stale_timeout_is_configurable_and_rejects_nonsense() {
        let _guard = isolated();
        let original = stale_timeout().num_seconds();
        assert_eq!(original, DEFAULT_STALE_TIMEOUT_SECS);

        set_stale_timeout_secs(0);
        set_stale_timeout_secs(-5);
        assert_eq!(
            stale_timeout().num_seconds(),
            original,
            "non-positive timeouts are ignored"
        );
    }
}
