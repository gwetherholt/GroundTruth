use chrono::Utc;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, warn};

mod api;
mod db;
mod metrics;
mod topics;
mod validation;

const RAW_BUFFER_TTL_SECS: u64 = 30;

type RawBuffer = Arc<Mutex<HashMap<String, (i64, Instant)>>>;

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
    let api_db = Arc::clone(&db);
    tokio::spawn(async move {
        api::serve(api_db, api_port).await;
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

    client.subscribe("groundtruth/#", QoS::AtLeastOnce).await?;
    info!(
        "Subscribed to groundtruth/# on {}:{}",
        broker_host, broker_port
    );

    loop {
        match eventloop.poll().await {
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
                    handle_value(&db, &raw_buffer, &reading, payload);
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

    let timestamp = Utc::now().to_rfc3339();

    let validation = if let Ok(db_lock) = db.lock() {
        validation::validate_reading(
            &db_lock,
            &reading.zone,
            &reading.zone_id,
            &reading.metric,
            value,
            raw_adc,
            &timestamp,
        )
    } else {
        validation::Validation::good()
    };

    if validation.quality != validation::Quality::Good {
        warn!(
            "{}: {} = {} flagged as {} ({})",
            reading.zone_id,
            reading.metric,
            value,
            validation.quality,
            validation.reason.as_deref().unwrap_or("?"),
        );
    }

    if let Ok(db_lock) = db.lock() {
        if let Err(e) = db::insert_reading(
            &db_lock,
            &reading.zone,
            &reading.zone_id,
            &reading.metric,
            value,
            &timestamp,
            raw_adc,
            validation.quality.as_str(),
            validation.reason.as_deref(),
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
        validation.quality.as_str(),
    );
}
