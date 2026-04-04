use chrono::Utc;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

mod db;
mod topics;

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

    let broker_host =
        std::env::var("MQTT_BROKER_HOST").unwrap_or_else(|_| "localhost".to_string());
    let broker_port: u16 = std::env::var("MQTT_BROKER_PORT")
        .unwrap_or_else(|_| "1883".to_string())
        .parse()
        .expect("MQTT_BROKER_PORT must be a valid u16");

    let mut mqtt_opts = MqttOptions::new("groundtruth-server", &broker_host, broker_port);
    mqtt_opts.set_keep_alive(std::time::Duration::from_secs(30));
    mqtt_opts.set_clean_session(true);

    let (client, mut eventloop) = AsyncClient::new(mqtt_opts, 64);

    // Subscribe to all GroundTruth topics
    client
        .subscribe("groundtruth/#", QoS::AtLeastOnce)
        .await?;
    info!(
        "Subscribed to groundtruth/# on {}:{}",
        broker_host, broker_port
    );

    // Process incoming MQTT messages
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

                match topics::parse_topic(topic) {
                    Some(reading) => {
                        let value: f64 = match payload.trim().parse() {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("Invalid numeric payload '{}' on {}: {}", payload, topic, e);
                                continue;
                            }
                        };

                        let timestamp = Utc::now().to_rfc3339();
                        if let Ok(db_lock) = db.lock() {
                            if let Err(e) = db::insert_reading(
                                &db_lock,
                                &reading.zone,
                                &reading.zone_id,
                                &reading.metric,
                                value,
                                &timestamp,
                            ) {
                                error!("DB insert failed: {}", e);
                            }
                        }
                    }
                    None => {
                        warn!("Unrecognized topic: {}", topic);
                    }
                }
            }
            Ok(_) => {} // Ignore other MQTT events (connack, pingresp, etc.)
            Err(e) => {
                error!("MQTT connection error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}
