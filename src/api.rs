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

pub type SharedDb = Arc<Mutex<Connection>>;

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

pub fn router(db: SharedDb) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/sensors", get(sensors_handler))
        .route("/api/readings/latest", get(latest_handler))
        .route("/api/readings/history", get(history_handler))
        .layer(cors)
        .with_state(db)
}

pub async fn serve(db: SharedDb, port: u16) {
    let app = router(db);
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

async fn sensors_handler(State(db): State<SharedDb>) -> Result<Json<Vec<Sensor>>, StatusCode> {
    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
    State(db): State<SharedDb>,
    Query(q): Query<LatestQuery>,
) -> Result<Json<Reading>, StatusCode> {
    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match query_latest(&conn, &q.zone, &q.zone_id, &q.metric) {
        Ok(r) => Ok(Json(r)),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

async fn history_handler(
    State(db): State<SharedDb>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<Reading>>, StatusCode> {
    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

fn internal_err(e: rusqlite::Error) -> StatusCode {
    error!("DB query error: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
}
