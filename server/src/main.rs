use chrono::{Duration, Utc};
use groundtruth_validator::{
    MetricConfig, QualityLevel, QuarantineTransition, Reading, StreamValidator, ValidatorConfig,
};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, warn};

mod api;
mod db;
mod metrics;
mod topics;

const RAW_BUFFER_TTL_SECS: u64 = 30;

type RawBuffer = Arc<Mutex<HashMap<String, (i64, Instant)>>>;
pub type SharedValidator = Arc<Mutex<StreamValidator>>;

/// Build the validator config that matches GroundTruth's sensor mix.
/// Numbers come from the original hardcoded `limits` module so behavior
/// stays equivalent to the pre-refactor server.
fn build_validator_config() -> ValidatorConfig {
    let moisture = MetricConfig::new(0.0..=100.0)
        .with_raw_range(100..=3995)
        .with_max_rate_of_change(30.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60));
    let humidity = MetricConfig::new(0.0..=100.0)
        .with_max_rate_of_change(30.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60));
    let temperature = MetricConfig::new(-40.0..=200.0)
        .with_max_rate_of_change(20.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60));

    ValidatorConfig::builder()
        .metric("moisture", moisture)
        .metric("humidity", humidity)
        .metric("temperature", temperature)
        .build()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "groundtruth=info".into()),
        )
        .init();

    info!("GroundTruth MQTT subscriber starting...");

    let db = db::init_db()?;
    let db = Arc::new(Mutex::new(db));
    let raw_buffer: RawBuffer = Arc::new(Mutex::new(HashMap::new()));

    let api_port: u16 = std::env::var("API_PORT")
        .unwrap_or_else(|_| "3001".to_string())
        .parse()
        .expect("API_PORT must be a valid u16");

    let validator: SharedValidator =
        Arc::new(Mutex::new(StreamValidator::new(build_validator_config())));

    let api_db = Arc::clone(&db);
    let api_validator = Arc::clone(&validator);
    tokio::spawn(async move {
        api::serve(api_db, api_validator, api_port).await;
    });

    let health_validator = Arc::clone(&validator);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let mut v = match health_validator.lock() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let scores = v.update_health();
            for (zone_full, metric, score) in scores.iter() {
                let (zone, zone_id) = split_source(zone_full);
                metrics::SENSOR_HEALTH_SCORE
                    .with_label_values(&[zone, zone_id, metric])
                    .set(score.overall);
            }
            let transitions = v.update_quarantine();
            for (zone_full, metric, transition) in transitions.iter() {
                let (zone, zone_id) = split_source(zone_full);
                let quarantined = v.is_quarantined(zone_full, metric);
                metrics::SENSOR_QUARANTINED
                    .with_label_values(&[zone, zone_id, metric])
                    .set(if quarantined { 1.0 } else { 0.0 });
                match transition {
                    QuarantineTransition::Entered => {
                        metrics::QUARANTINE_EVENTS_TOTAL
                            .with_label_values(&[zone, zone_id, metric])
                            .inc();
                        warn!("Sensor {}/{} entered quarantine", zone_full, metric);
                    }
                    QuarantineTransition::Recovered => {
                        info!("Sensor {}/{} recovered from quarantine", zone_full, metric);
                    }
                    QuarantineTransition::Unchanged => {}
                }
            }
            tracing::debug!("Refreshed health for {} sensors", scores.len());
        }
    });

    let broker_host = std::env::var("MQTT_BROKER_HOST").unwrap_or_else(|_| "localhost".to_string());
    let broker_port: u16 = std::env::var("MQTT_BROKER_PORT")
        .unwrap_or_else(|_| "1883".to_string())
        .parse()
        .expect("MQTT_BROKER_PORT must be a valid u16");

    let mut mqtt_opts = MqttOptions::new("groundtruth-server", &broker_host, broker_port);
    mqtt_opts.set_keep_alive(std::time::Duration::from_secs(30));
    mqtt_opts.set_clean_session(true);

    let (client, mut eventloop) = AsyncClient::new(mqtt_opts, 64);

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!(
                    "MQTT (re)connected to {}:{} — subscribing to groundtruth/#",
                    broker_host, broker_port
                );
                if let Err(e) = client.subscribe("groundtruth/#", QoS::AtLeastOnce).await {
                    error!("Failed to (re)subscribe: {}", e);
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let topic = &publish.topic;
                let payload = match std::str::from_utf8(&publish.payload) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Invalid UTF-8 payload on {}: {}", topic, e);
                        continue;
                    }
                };

                info!("Received: {} = {}", topic, payload);

                let reading = match topics::parse_topic(topic) {
                    Some(r) => r,
                    None => {
                        warn!("Unrecognized topic: {}", topic);
                        continue;
                    }
                };

                if reading.is_raw_adc() {
                    handle_raw_adc(&raw_buffer, &reading, payload);
                } else {
                    handle_value(&db, &validator, &raw_buffer, &reading, payload);
                }
            }
            Ok(_) => {}
            Err(e) => {
                error!("MQTT connection error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

fn handle_raw_adc(buffer: &RawBuffer, reading: &topics::TopicReading, payload: &str) {
    let raw: i64 = match payload.trim().parse() {
        Ok(v) => v,
        Err(e) => {
            warn!("Invalid raw_adc payload '{}': {}", payload, e);
            return;
        }
    };
    let key = format!("{}/{}", reading.zone, reading.zone_id);
    if let Ok(mut buf) = buffer.lock() {
        buf.insert(key, (raw, Instant::now()));
    }
}

fn handle_value(
    db: &Arc<Mutex<rusqlite::Connection>>,
    validator: &SharedValidator,
    buffer: &RawBuffer,
    reading: &topics::TopicReading,
    payload: &str,
) {
    let value: f64 = match payload.trim().parse() {
        Ok(v) => v,
        Err(e) => {
            warn!("Invalid numeric payload '{}': {}", payload, e);
            return;
        }
    };

    let raw_adc = if reading.metric == "moisture" {
        let key = format!("{}/{}", reading.zone, reading.zone_id);
        if let Ok(mut buf) = buffer.lock() {
            buf.retain(|_, (_, ts)| ts.elapsed().as_secs() < RAW_BUFFER_TTL_SECS);
            buf.remove(&key).map(|(raw, _)| raw)
        } else {
            None
        }
    } else {
        None
    };

    let now = Utc::now();
    let source = format!("{}/{}", reading.zone, reading.zone_id);
    let mut gt_reading = Reading::new(&source, &reading.metric, value, now);
    if let Some(raw) = raw_adc {
        gt_reading = gt_reading.with_raw(raw);
    }

    let result = match validator.lock() {
        Ok(mut v) => v.validate(gt_reading),
        Err(_) => groundtruth_validator::ValidationResult::good(),
    };

    if result.quality != QualityLevel::Good {
        warn!(
            "{}: {} = {} flagged as {} ({})",
            reading.zone_id,
            reading.metric,
            value,
            result.quality,
            if result.reason.is_empty() {
                "?"
            } else {
                &result.reason
            },
        );
    }

    let timestamp = now.to_rfc3339();
    let reason_for_db = if result.quality == QualityLevel::Good {
        None
    } else {
        Some(result.reason.as_str())
    };

    if let Ok(db_lock) = db.lock() {
        if let Err(e) = db::insert_reading(
            &db_lock,
            &reading.zone,
            &reading.zone_id,
            &reading.metric,
            value,
            &timestamp,
            raw_adc,
            result.quality.as_str(),
            reason_for_db,
        ) {
            error!("DB insert failed: {}", e);
        }
    }

    metrics::record_reading(
        &reading.zone,
        &reading.zone_id,
        &reading.metric,
        value,
        raw_adc,
        result.quality.as_str(),
    );
}

/// Split a validator `source` of the form `"{zone}/{zone_id}"` back
/// into its parts. Returns `(source, source)` as a fallback if the
/// source doesn't contain `/`.
pub fn split_source(source: &str) -> (&str, &str) {
    match source.split_once('/') {
        Some((z, id)) => (z, id),
        None => (source, source),
    }
}
