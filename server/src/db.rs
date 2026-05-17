use rusqlite::{params, Connection, Result};
use tracing::info;

/// Initialize the SQLite database and create tables if they don't exist.
pub fn init_db() -> Result<Connection> {
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "groundtruth.db".to_string());
    let conn = Connection::open(&db_path)?;
    create_schema(&conn)?;
    info!("Database initialized at {}", db_path);
    Ok(conn)
}

/// Create the schema. Extracted so tests can use the same DDL as production
/// without duplicating it. Uses additive migrations via ALTER TABLE so existing
/// databases pick up new columns without losing data.
pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS readings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            zone TEXT NOT NULL,
            zone_id TEXT NOT NULL,
            metric TEXT NOT NULL,
            value REAL NOT NULL,
            timestamp TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_readings_zone
            ON readings (zone, zone_id, metric);

        CREATE INDEX IF NOT EXISTS idx_readings_timestamp
            ON readings (timestamp);

        CREATE TABLE IF NOT EXISTS beds (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS alerts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            zone TEXT NOT NULL,
            zone_id TEXT NOT NULL,
            metric TEXT NOT NULL,
            threshold_low REAL,
            threshold_high REAL,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        ",
    )?;

    add_column_if_missing(conn, "readings", "raw_adc", "INTEGER")?;
    add_column_if_missing(conn, "readings", "quality", "TEXT NOT NULL DEFAULT 'good'")?;
    add_column_if_missing(conn, "readings", "validation_reason", "TEXT")?;

    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    column_def: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();

    if !existing.iter().any(|c| c == column) {
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, column_def),
            [],
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_reading(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    value: f64,
    timestamp: &str,
    raw_adc: Option<i64>,
    quality: &str,
    validation_reason: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO readings
            (zone, zone_id, metric, value, timestamp, raw_adc, quality, validation_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            zone,
            zone_id,
            metric,
            value,
            timestamp,
            raw_adc,
            quality,
            validation_reason
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_db_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM readings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_insert_and_query_reading() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        insert_reading(
            &conn,
            "bed",
            "1",
            "moisture",
            42.5,
            "2026-04-04T12:00:00Z",
            Some(2400),
            "good",
            None,
        )
        .unwrap();

        let (zone, value, raw_adc, quality): (String, f64, Option<i64>, String) = conn
            .query_row(
                "SELECT zone, value, raw_adc, quality FROM readings WHERE zone_id = '1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(zone, "bed");
        assert!((value - 42.5).abs() < f64::EPSILON);
        assert_eq!(raw_adc, Some(2400));
        assert_eq!(quality, "good");
    }

    #[test]
    fn test_migration_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
    }

    #[test]
    fn test_dht22_reading_without_raw_adc() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        insert_reading(
            &conn,
            "bed",
            "1",
            "temperature",
            72.5,
            "2026-04-04T12:00:00Z",
            None,
            "good",
            None,
        )
        .unwrap();

        let raw_adc: Option<i64> = conn
            .query_row(
                "SELECT raw_adc FROM readings WHERE metric = 'temperature'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_adc, None);
    }
}
