use chrono::{DateTime, Duration, Utc};
use groundtruth_validator::{
    MetricConfig, QualityLevel, QuarantineTransition, Reading, SourceGroupConfig, StreamValidator,
    ValidatorConfig,
};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, warn};

use topics::{
    CHARSTATION_CONTROL_PREFIX, CHARSTATION_MAX_UNIT, CHARSTATION_SESSION_TOPIC, CHARSTATION_ZONE,
};

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
///
/// Stuck detection is the one place the sensors genuinely differ. The
/// SEN0308 soil probe is a raw analog part with a ±25-count ADC noise
/// floor, so six consecutive readings within ±0.01 really is broken.
/// The DHT22 reports in 0.1 steps and watches a garage that moves
/// ~1 °F/hour, so identical consecutive readings are its normal state —
/// what's abnormal there is the value not moving for an hour.
fn build_validator_config() -> ValidatorConfig {
    // Moisture keeps the inherited 0.01 `stuck_threshold`, which on a
    // calibrated percentage is an exact-repeat detector rather than a
    // noise band. It works because ADC noise makes exact repeats rare,
    // but it was never derived from the SEN0308's measured noise floor.
    // That is the first number the characterization station owes us —
    // see "Where this is going" in the top-level README.
    let moisture = MetricConfig::new(0.0..=100.0)
        .with_raw_range(100..=3995)
        .with_max_rate_of_change(30.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60))
        .with_stuck_defaults_for("moisture");
    let humidity = MetricConfig::new(0.0..=100.0)
        .with_max_rate_of_change(30.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60))
        .with_stuck_defaults_for("humidity");
    let temperature = MetricConfig::new(-40.0..=200.0)
        .with_max_rate_of_change(20.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60))
        .with_stuck_defaults_for("temperature");

    // Raw ADC counts as a metric in their own right. The rate ceiling
    // is the calibrated-moisture one (30 %/600 s) expressed in counts:
    // bed/1's span runs SOIL_RAW_DRY 2800 -> SOIL_RAW_WET 1200, i.e.
    // 1600 counts per 100 %, so 30 % is about 480 counts. Nothing in
    // the garden publishes this metric today; it is the baseline the
    // bench overrides below, and where a garden raw-ADC stream would
    // land if one were added.
    let raw_adc = MetricConfig::new(0.0..=4095.0)
        .with_raw_range(100..=3995)
        .with_max_rate_of_change(480.0)
        .with_rate_window(Duration::seconds(600))
        .with_expected_cadence(Duration::seconds(60))
        .with_stuck_defaults_for("raw_adc");

    // The characterization station is a bench fixture, not a deployed
    // sensor. Units are plugged and unplugged constantly and go silent
    // for weeks between sessions, so Tier-2 health and Tier-3
    // quarantine — both of which read silence as failure — would call
    // every unit broken. Tier-1 still applies: a shorted probe reads
    // out of range on the bench exactly as it does in a bed.
    let charstation_raw_adc = raw_adc
        .clone()
        // Pulling the probe out of sand and into water between two
        // readings is the experiment, not an anomaly.
        .without_rate_check()
        .with_expected_cadence(Duration::seconds(2));

    ValidatorConfig::builder()
        .metric("moisture", moisture)
        .metric("humidity", humidity)
        .metric("temperature", temperature)
        .metric("raw_adc", raw_adc)
        .source_group(
            CHARSTATION_ZONE,
            SourceGroupConfig::tier1_only().metric("raw_adc", charstation_raw_adc),
        )
        .build()
}

/// One `charstation/session` message: the bench conditions in force
/// from the moment it is received until the next one arrives.
///
/// Typed loosely on purpose — this is hand-published from the Pi with
/// `mosquitto_pub` in the middle of a protocol run, so `unit_id` is
/// accepted as either `"3"` or `3`, and everything but the unit and
/// the batch is optional.
#[derive(Debug, Deserialize)]
struct SessionMessage {
    unit_id: UnitId,
    batch: String,
    #[serde(default)]
    insertion: Option<i64>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UnitId {
    Text(String),
    Number(i64),
}

impl UnitId {
    fn as_string(&self) -> String {
        match self {
            UnitId::Text(s) => s.trim().to_string(),
            UnitId::Number(n) => n.to_string(),
        }
    }
}

/// Parse a session message and write it to the database, stamped at
/// `received_at`. Returns the new row id, or `None` if the payload
/// could not be used.
///
/// A bad payload is logged and dropped, never fatal: the station is
/// driven by hand from a terminal, and a typo at the bench must not
/// take the ingest loop down.
fn handle_session_message(
    db: &Arc<Mutex<rusqlite::Connection>>,
    payload: &str,
    received_at: DateTime<Utc>,
) -> Option<i64> {
    let json: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Ignoring malformed {} payload '{}': {}",
                CHARSTATION_SESSION_TOPIC, payload, e
            );
            return None;
        }
    };
    // Insist on an object. Serde will happily fill a struct from a
    // JSON *array* by position, so `["unit_id", "3"]` would otherwise
    // become a session row for a unit called "unit_id".
    if !json.is_object() {
        warn!(
            "Ignoring {} payload '{}': expected a JSON object",
            CHARSTATION_SESSION_TOPIC, payload
        );
        return None;
    }
    let msg: SessionMessage = match serde_json::from_value(json) {
        Ok(m) => m,
        Err(e) => {
            warn!(
                "Ignoring unusable {} payload '{}': {}",
                CHARSTATION_SESSION_TOPIC, payload, e
            );
            return None;
        }
    };

    let unit_id = msg.unit_id.as_string();
    if unit_id.is_empty() || msg.batch.trim().is_empty() {
        warn!(
            "Ignoring {} payload '{}': unit_id and batch must both be non-empty",
            CHARSTATION_SESSION_TOPIC, payload
        );
        return None;
    }
    if !topics::is_charstation_unit(&unit_id) {
        // Logged, not rejected. The row is the only record of what was
        // on the bench, so dropping it would lose data that exists
        // nowhere else; it simply won't line up with a readings stream.
        warn!(
            "{}: unit_id '{}' is outside 1..={} — recorded anyway, but no station stream will match it",
            CHARSTATION_SESSION_TOPIC, unit_id, CHARSTATION_MAX_UNIT
        );
    }

    let started_at = received_at.to_rfc3339();
    let batch = msg.batch.trim();
    let notes = msg
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());

    let db_lock = match db.lock() {
        Ok(l) => l,
        Err(_) => {
            error!("Session insert skipped: database mutex poisoned");
            return None;
        }
    };
    match db::insert_charstation_session(
        &db_lock,
        &started_at,
        &unit_id,
        batch,
        msg.insertion,
        notes,
    ) {
        Ok(id) => {
            info!(
                "Charstation session {}: unit {} in '{}' (insertion {:?}) at {}",
                id, unit_id, batch, msg.insertion, started_at
            );
            Some(id)
        }
        Err(e) => {
            error!("Session insert failed: {}", e);
            None
        }
    }
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

    if let Ok(raw) = std::env::var("STALE_TIMEOUT_SECS") {
        match raw.parse::<i64>() {
            Ok(secs) if secs > 0 => metrics::set_stale_timeout_secs(secs),
            _ => warn!(
                "Ignoring STALE_TIMEOUT_SECS='{}' — expected a positive integer",
                raw
            ),
        }
    }
    info!(
        "Value gauges stop being exported after {}s of stream silence",
        metrics::stale_timeout().num_seconds()
    );

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
            // Age gauges climb on the tick, not only on arrival, so a
            // silent stream's silence is itself visible in Prometheus.
            metrics::refresh_stale_gauges(Utc::now());

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
                // Subscriptions are re-issued on every ConnAck, not
                // just the first: a broker restart drops them silently
                // and the stream would stay quiet forever otherwise.
                info!(
                    "MQTT (re)connected to {}:{} — subscribing to groundtruth/# and charstation/#",
                    broker_host, broker_port
                );
                for filter in ["groundtruth/#", "charstation/#"] {
                    if let Err(e) = client.subscribe(filter, QoS::AtLeastOnce).await {
                        error!("Failed to (re)subscribe to {}: {}", filter, e);
                    }
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

                if topic == CHARSTATION_SESSION_TOPIC {
                    handle_session_message(&db, payload, Utc::now());
                    continue;
                }
                if topic.starts_with(CHARSTATION_CONTROL_PREFIX) {
                    // Retained station control state (the unit under
                    // test). Meant for the station firmware; the server
                    // sees it only because it subscribes to the whole
                    // namespace.
                    tracing::debug!("Ignoring station control topic {}", topic);
                    continue;
                }

                let reading = match topics::parse_topic(topic) {
                    Some(r) => r,
                    None => {
                        warn!("Unrecognized topic: {}", topic);
                        continue;
                    }
                };

                if reading.is_buffered_raw() {
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

    let raw_adc = match reading.metric.as_str() {
        // Garden nodes publish the raw counts on their own topic a
        // moment before the calibrated value; pair them back up.
        "moisture" => {
            let key = format!("{}/{}", reading.zone, reading.zone_id);
            if let Ok(mut buf) = buffer.lock() {
                buf.retain(|_, (_, ts)| ts.elapsed().as_secs() < RAW_BUFFER_TTL_SECS);
                buf.remove(&key).map(|(raw, _)| raw)
            } else {
                None
            }
        }
        // On the station the reading *is* the raw counts. Storing them
        // in the raw_adc column as well means the study reads one
        // column whichever zone a row came from, and it gives Tier-1's
        // raw-range rule something to check.
        "raw_adc" => Some(value.round() as i64),
        _ => None,
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

#[cfg(test)]
mod config_tests {
    use super::*;
    use groundtruth_validator::{QualityLevel, StreamPolicy, StuckMode};

    #[test]
    fn dht22_metrics_use_time_based_stuck_detection() {
        let cfg = build_validator_config();
        for metric in ["temperature", "humidity"] {
            let mc = cfg.metric(metric).expect(metric);
            assert_eq!(mc.stuck_mode, StuckMode::Duration, "{metric}");
            assert_eq!(mc.resolution, Some(0.1), "{metric}");
            assert_eq!(mc.stuck_window, Duration::minutes(60), "{metric}");
        }
    }

    #[test]
    fn soil_probe_keeps_count_based_stuck_detection() {
        let cfg = build_validator_config();
        let mc = cfg.metric("moisture").expect("moisture");
        assert_eq!(mc.stuck_mode, StuckMode::Count);
        assert_eq!(mc.stuck_count, 6);
        assert_eq!(mc.stuck_threshold, 0.01);
    }

    #[test]
    fn charstation_is_tier1_only_and_the_garden_is_not() {
        let cfg = build_validator_config();
        assert_eq!(cfg.policy_for("charstation/3"), StreamPolicy::Tier1Only);
        assert_eq!(cfg.policy_for("bed/1"), StreamPolicy::Full);
        assert_eq!(cfg.policy_for("greenhouse/greenhouse"), StreamPolicy::Full);
    }

    #[test]
    fn charstation_raw_adc_keeps_tier1_but_drops_the_rate_check() {
        let cfg = build_validator_config();
        let mc = cfg
            .metric_for("charstation/3", "raw_adc")
            .expect("charstation raw_adc");

        // Rate-of-change off: insertion and removal are step changes
        // by design.
        assert!(mc.max_rate_of_change.is_infinite());
        // Raw range and count-based stuck detection still on.
        assert_eq!(mc.raw_valid_range, Some(100..=3995));
        assert_eq!(mc.stuck_mode, StuckMode::Count);
        assert_eq!(mc.stuck_count, 6);
        assert_eq!(mc.expected_cadence, Duration::seconds(2));

        // The garden baseline for the same metric keeps its ceiling.
        assert_eq!(
            cfg.metric_for("bed/1", "raw_adc")
                .expect("garden raw_adc")
                .max_rate_of_change,
            480.0
        );
    }

    #[test]
    fn charstation_temperature_uses_the_bed_temperature_defaults() {
        let cfg = build_validator_config();
        let bench = cfg
            .metric_for("charstation/3", "temperature")
            .expect("bench temperature");
        let bed = cfg
            .metric_for("bed/1", "temperature")
            .expect("bed temperature");

        assert_eq!(bench.valid_range, bed.valid_range);
        assert_eq!(bench.max_rate_of_change, bed.max_rate_of_change);
        assert_eq!(bench.stuck_mode, bed.stuck_mode);
        assert_eq!(bench.stuck_window, bed.stuck_window);
        assert_eq!(bench.resolution, bed.resolution);
    }

    #[test]
    fn charstation_streams_never_gain_health_or_quarantine_state() {
        let mut v = StreamValidator::new(build_validator_config());
        let now = Utc::now();

        // A session: 30 readings, then three weeks of silence and
        // repeated health ticks.
        for i in 0..30i64 {
            v.validate(Reading::new(
                "charstation/3",
                "raw_adc",
                1500.0 + (i % 11) as f64,
                now - Duration::days(21) + Duration::seconds(2 * i),
            ));
        }
        v.validate(Reading::new("bed/1", "moisture", 42.0, now));

        let scores = v.update_health();
        assert!(
            scores.iter().all(|(src, _, _)| src != "charstation/3"),
            "bench streams must not be health-scored"
        );
        let transitions = v.update_quarantine();
        assert!(transitions.iter().all(|(src, _, _)| src != "charstation/3"));
        assert!(!v.is_quarantined("charstation/3", "raw_adc"));
        assert!(v.quarantine_status("charstation/3", "raw_adc").is_none());
    }

    #[test]
    fn charstation_raw_adc_still_gets_tier1_verdicts() {
        let mut v = StreamValidator::new(build_validator_config());
        let now = Utc::now();

        // A shorted probe is still invalid on the bench.
        let res = v.validate(Reading::new("charstation/2", "raw_adc", 40.0, now).with_raw(40));
        assert_eq!(res.quality, QualityLevel::Invalid);
        assert_eq!(res.rule, "raw_range");

        // Air (3000) straight into water (1200) is fine here.
        let mut v = StreamValidator::new(build_validator_config());
        v.validate(Reading::new("charstation/2", "raw_adc", 3000.0, now).with_raw(3000));
        let res = v.validate(
            Reading::new(
                "charstation/2",
                "raw_adc",
                1200.0,
                now + Duration::seconds(2),
            )
            .with_raw(1200),
        );
        assert_eq!(res.quality, QualityLevel::Good, "{}", res.reason);
    }
}

#[cfg(test)]
mod ingest_tests {
    use super::*;

    fn fixture() -> (Arc<Mutex<rusqlite::Connection>>, SharedValidator, RawBuffer) {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        db::create_schema(&conn).expect("schema");
        (
            Arc::new(Mutex::new(conn)),
            Arc::new(Mutex::new(StreamValidator::new(build_validator_config()))),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    #[test]
    fn charstation_raw_adc_is_stored_as_a_reading() {
        // The garden's `moisture_raw` is buffered and attached to the
        // calibrated value that follows. The station's `raw_adc` has
        // no calibrated value to attach to — it is the measurement.
        let (db, validator, buffer) = fixture();
        let reading = topics::parse_topic("charstation/3/raw_adc").expect("topic");
        assert!(!reading.is_buffered_raw());

        handle_value(&db, &validator, &buffer, &reading, "1487");

        let conn = db.lock().unwrap();
        let (zone, zone_id, metric, value, raw_adc, quality): (
            String,
            String,
            String,
            f64,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "SELECT zone, zone_id, metric, value, raw_adc, quality FROM readings",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(zone, "charstation");
        assert_eq!(zone_id, "3");
        assert_eq!(metric, "raw_adc");
        assert_eq!(value, 1487.0);
        // Stored in the raw_adc column too, so the study reads one
        // column whichever zone a row came from.
        assert_eq!(raw_adc, Some(1487));
        assert_eq!(quality, "good");
    }

    #[test]
    fn an_out_of_range_station_reading_is_persisted_as_invalid() {
        // Classify, don't destroy: a shorted probe on the bench is
        // still a row, flagged, with a reason.
        let (db, validator, buffer) = fixture();
        let reading = topics::parse_topic("charstation/3/raw_adc").expect("topic");

        handle_value(&db, &validator, &buffer, &reading, "12");

        let conn = db.lock().unwrap();
        let (quality, reason): (String, Option<String>) = conn
            .query_row("SELECT quality, validation_reason FROM readings", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(quality, "invalid");
        assert!(reason.unwrap().contains("raw value"));
    }

    #[test]
    fn a_station_session_leaves_no_health_or_quarantine_state() {
        let (db, validator, buffer) = fixture();
        let reading = topics::parse_topic("charstation/3/raw_adc").expect("topic");
        for counts in ["3000", "2980", "1250", "1240", "1245"] {
            handle_value(&db, &validator, &buffer, &reading, counts);
        }

        let mut v = validator.lock().unwrap();
        assert!(v.update_health().is_empty());
        assert!(v.update_quarantine().is_empty());
        assert!(v.sources().is_empty());
    }

    #[test]
    fn a_garden_reading_still_pairs_with_its_buffered_raw() {
        // Regression guard on the rename: the bed path must be
        // untouched by the station's arrival.
        let (db, validator, buffer) = fixture();
        let raw = topics::parse_topic("groundtruth/bed/1/moisture_raw").expect("topic");
        assert!(raw.is_buffered_raw());
        handle_raw_adc(&buffer, &raw, "2400");

        let moisture = topics::parse_topic("groundtruth/bed/1/moisture").expect("topic");
        handle_value(&db, &validator, &buffer, &moisture, "42.5");

        let conn = db.lock().unwrap();
        let (metric, raw_adc): (String, Option<i64>) = conn
            .query_row("SELECT metric, raw_adc FROM readings", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(metric, "moisture");
        assert_eq!(raw_adc, Some(2400));

        // And the bed is health-scored, unlike the bench.
        let mut v = validator.lock().unwrap();
        assert_eq!(v.update_health().len(), 1);
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn test_db() -> Arc<Mutex<rusqlite::Connection>> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        db::create_schema(&conn).expect("schema");
        Arc::new(Mutex::new(conn))
    }

    fn rows(
        db: &Arc<Mutex<rusqlite::Connection>>,
    ) -> Vec<(String, String, Option<i64>, Option<String>)> {
        let conn = db.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT unit_id, batch_label, insertion_number, notes
                 FROM charstation_sessions ORDER BY id",
            )
            .unwrap();
        let out = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        out
    }

    #[test]
    fn session_message_inserts_a_row_stamped_at_receipt() {
        let db = test_db();
        let now = Utc::now();
        let id = handle_session_message(
            &db,
            r#"{"unit_id": "3", "batch": "sand-100gkg", "insertion": 4, "notes": "reseated"}"#,
            now,
        );
        assert!(id.is_some());

        assert_eq!(
            rows(&db),
            vec![(
                "3".to_string(),
                "sand-100gkg".to_string(),
                Some(4),
                Some("reseated".to_string())
            )]
        );

        let conn = db.lock().unwrap();
        let started_at: String = conn
            .query_row("SELECT started_at FROM charstation_sessions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(started_at, now.to_rfc3339());
    }

    #[test]
    fn session_message_accepts_a_numeric_unit_id_and_omitted_optionals() {
        let db = test_db();
        assert!(
            handle_session_message(&db, r#"{"unit_id": 7, "batch": "air"}"#, Utc::now()).is_some()
        );
        assert_eq!(
            rows(&db),
            vec![("7".to_string(), "air".to_string(), None, None)]
        );
    }

    #[test]
    fn malformed_session_payloads_are_logged_and_dropped() {
        let db = test_db();
        for payload in [
            "",
            "not json at all",
            "{",
            r#"{"unit_id": "3"}"#,                // no batch
            r#"{"batch": "air"}"#,                // no unit
            r#"{"unit_id": "", "batch": "air"}"#, // empty unit
            r#"{"unit_id": "3", "batch": "  "}"#, // blank batch
            r#"{"unit_id": "3", "batch": "air", "insertion": "four"}"#, // wrong type
            r#"["unit_id", "3"]"#,                // not an object
            "null",
        ] {
            assert!(
                handle_session_message(&db, payload, Utc::now()).is_none(),
                "payload {payload:?} should have been dropped"
            );
        }
        assert!(rows(&db).is_empty(), "nothing should have been written");
    }

    #[test]
    fn out_of_range_unit_is_recorded_rather_than_lost() {
        // It cannot line up with any charstation/{1..16} stream, but
        // it is still the only record of what was on the bench.
        let db = test_db();
        assert!(
            handle_session_message(&db, r#"{"unit_id": "42", "batch": "water"}"#, Utc::now())
                .is_some()
        );
        assert_eq!(rows(&db).len(), 1);
    }

    #[test]
    fn consecutive_sessions_append_in_receipt_order() {
        let db = test_db();
        let t0 = Utc::now();
        for (offset, batch) in [(0i64, "air"), (600, "sand-dry"), (1200, "water")] {
            let payload = format!(r#"{{"unit_id": "3", "batch": "{batch}"}}"#);
            handle_session_message(&db, &payload, t0 + Duration::seconds(offset)).unwrap();
        }
        let batches: Vec<String> = rows(&db).into_iter().map(|r| r.1).collect();
        assert_eq!(batches, vec!["air", "sand-dry", "water"]);
    }
}
