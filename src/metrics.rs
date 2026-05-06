use once_cell::sync::Lazy;
use prometheus::{Encoder, GaugeVec, IntCounterVec, Registry, TextEncoder};

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

pub fn record_reading(
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

    if quality == "good" {
        match metric {
            "moisture" => {
                MOISTURE_PCT.with_label_values(&[zone, zone_id]).set(value);
                if let Some(raw) = raw_adc {
                    MOISTURE_RAW
                        .with_label_values(&[zone, zone_id])
                        .set(raw as f64);
                }
            }
            "temperature" => {
                TEMPERATURE_F.with_label_values(&[zone, zone_id]).set(value);
            }
            "humidity" => {
                HUMIDITY_PCT.with_label_values(&[zone, zone_id]).set(value);
            }
            _ => {}
        }
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
