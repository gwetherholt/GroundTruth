use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};

use crate::sensor_health::{HealthReport, SharedHealthCache, SharedQuarantineCache};

pub type SharedDb = Arc<Mutex<Connection>>;

#[derive(Clone)]
pub struct AppState {
    pub db: SharedDb,
    pub health: SharedHealthCache,
    pub quarantine: SharedQuarantineCache,
}

#[derive(Serialize)]
pub struct QuarantinedSensor {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
    pub quarantined_at: chrono::DateTime<chrono::Utc>,
    pub reason: String,
    pub current_health_score: f64,
}

#[derive(Serialize)]
pub struct QuarantineResponse {
    pub quarantined_sensors: Vec<QuarantinedSensor>,
    pub total_active_sensors: usize,
    pub total_quarantined: usize,
}

#[derive(Serialize, Deserialize)]
pub struct Reading {
    pub id: i64,
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
    pub value: f64,
    pub raw_adc: Option<i64>,
    pub quality: String,
    pub validation_reason: Option<String>,
    pub timestamp: String,
}

#[derive(Serialize, Deserialize)]
pub struct Sensor {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
    pub latest: Option<Reading>,
}

#[derive(Deserialize)]
pub struct LatestQuery {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub zone: String,
    pub zone_id: String,
    pub metric: String,
    #[serde(default = "default_hours")]
    pub hours: u32,
}

fn default_hours() -> u32 {
    24
}

pub fn router(
    db: SharedDb,
    health: SharedHealthCache,
    quarantine: SharedQuarantineCache,
) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/sensors", get(sensors_handler))
        .route("/api/readings/latest", get(latest_handler))
        .route("/api/readings/history", get(history_handler))
        .route("/api/sensor-health", get(sensor_health_handler))
        .route("/api/sensor-health/quarantine", get(quarantine_handler))
        .route("/metrics", get(metrics_handler))
        .layer(cors)
        .with_state(AppState {
            db,
            health,
            quarantine,
        })
}

async fn metrics_handler() -> (
    axum::http::StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    String,
) {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        crate::metrics::render(),
    )
}

pub async fn serve(
    db: SharedDb,
    health: SharedHealthCache,
    quarantine: SharedQuarantineCache,
    port: u16,
) {
    let app = router(db, health, quarantine);
    let addr = format!("0.0.0.0:{}", port);
    info!("HTTP API listening on {}", addr);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind {}: {}", addr, e);
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        error!("HTTP server error: {}", e);
    }
}

async fn sensors_handler(State(state): State<AppState>) -> Result<Json<Vec<Sensor>>, StatusCode> {
    let conn = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT zone, zone_id, metric FROM readings ORDER BY zone, zone_id, metric",
        )
        .map_err(internal_err)?;

    let sensor_keys: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(internal_err)?
        .filter_map(|r| r.ok())
        .collect();

    drop(stmt);

    let mut sensors = Vec::with_capacity(sensor_keys.len());
    for (zone, zone_id, metric) in sensor_keys {
        let latest = query_latest(&conn, &zone, &zone_id, &metric).ok();
        sensors.push(Sensor {
            zone,
            zone_id,
            metric,
            latest,
        });
    }

    Ok(Json(sensors))
}

async fn latest_handler(
    State(state): State<AppState>,
    Query(q): Query<LatestQuery>,
) -> Result<Json<Reading>, StatusCode> {
    let conn = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match query_latest(&conn, &q.zone, &q.zone_id, &q.metric) {
        Ok(r) => Ok(Json(r)),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

async fn history_handler(
    State(state): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<Reading>>, StatusCode> {
    let conn = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(q.hours as i64)).to_rfc3339();

    let mut stmt = conn
        .prepare(
            "SELECT id, zone, zone_id, metric, value, raw_adc, quality, validation_reason, timestamp
             FROM readings
             WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3 AND timestamp >= ?4
             ORDER BY id ASC",
        )
        .map_err(internal_err)?;

    let rows = stmt
        .query_map(
            rusqlite::params![q.zone, q.zone_id, q.metric, cutoff],
            row_to_reading,
        )
        .map_err(internal_err)?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    Ok(Json(rows))
}

fn query_latest(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
) -> rusqlite::Result<Reading> {
    conn.query_row(
        "SELECT id, zone, zone_id, metric, value, raw_adc, quality, validation_reason, timestamp
         FROM readings
         WHERE zone = ?1 AND zone_id = ?2 AND metric = ?3
         ORDER BY id DESC LIMIT 1",
        rusqlite::params![zone, zone_id, metric],
        row_to_reading,
    )
}

fn row_to_reading(row: &rusqlite::Row) -> rusqlite::Result<Reading> {
    Ok(Reading {
        id: row.get(0)?,
        zone: row.get(1)?,
        zone_id: row.get(2)?,
        metric: row.get(3)?,
        value: row.get(4)?,
        raw_adc: row.get(5)?,
        quality: row.get(6)?,
        validation_reason: row.get(7)?,
        timestamp: row.get(8)?,
    })
}

async fn quarantine_handler(State(state): State<AppState>) -> Json<QuarantineResponse> {
    let health_scores: std::collections::HashMap<_, _> = match state.health.lock() {
        Ok(c) => c.iter().map(|(k, r)| (k.clone(), r.score)).collect(),
        Err(_) => std::collections::HashMap::new(),
    };

    let quarantine_entries: Vec<_> = match state.quarantine.lock() {
        Ok(q) => q
            .iter()
            .filter(|(_, s)| s.is_quarantined)
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect(),
        Err(_) => Vec::new(),
    };

    let total_active_sensors = health_scores.len();
    let total_quarantined = quarantine_entries.len();

    let mut quarantined_sensors: Vec<QuarantinedSensor> = quarantine_entries
        .into_iter()
        .filter_map(|(key, state)| {
            let quarantined_at = state.quarantined_at?;
            let reason = state.reason.clone().unwrap_or_default();
            let current_health_score = health_scores.get(&key).copied().unwrap_or(0.0);
            Some(QuarantinedSensor {
                zone: key.zone,
                zone_id: key.zone_id,
                metric: key.metric,
                quarantined_at,
                reason,
                current_health_score,
            })
        })
        .collect();

    // Stable ordering: worst score first.
    quarantined_sensors.sort_by(|a, b| {
        a.current_health_score
            .partial_cmp(&b.current_health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Json(QuarantineResponse {
        quarantined_sensors,
        total_active_sensors,
        total_quarantined,
    })
}

async fn sensor_health_handler(State(state): State<AppState>) -> Json<Vec<HealthReport>> {
    let mut reports: Vec<HealthReport> = match state.health.lock() {
        Ok(c) => c.values().cloned().collect(),
        Err(_) => return Json(Vec::new()),
    };
    // Sort by score ascending so worst sensors appear first
    reports.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Json(reports)
}

fn internal_err(e: rusqlite::Error) -> StatusCode {
    error!("DB query error: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
}
