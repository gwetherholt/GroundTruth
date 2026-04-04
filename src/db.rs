use rusqlite::{params, Connection, Result};
use tracing::info;

/// Initialize the SQLite database and create tables if they don't exist.
pub fn init_db() -> Result<Connection> {
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "groundtruth.db".to_string());
    let conn = Connection::open(&db_path)?;

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

    info!("Database initialized at {}", db_path);
    Ok(conn)
}

/// Insert a sensor reading into the database.
pub fn insert_reading(
    conn: &Connection,
    zone: &str,
    zone_id: &str,
    metric: &str,
    value: f64,
    timestamp: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO readings (zone, zone_id, metric, value, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![zone, zone_id, metric, value, timestamp],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_db_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
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
            ",
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM readings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_insert_and_query_reading() {
        let conn = Connection::open_in_memory().unwrap();
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
            ",
        )
        .unwrap();

        insert_reading(&conn, "bed", "1", "moisture", 42.5, "2026-04-04T12:00:00Z").unwrap();

        let (zone, value): (String, f64) = conn
            .query_row(
                "SELECT zone, value FROM readings WHERE zone_id = '1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(zone, "bed");
        assert!((value - 42.5).abs() < f64::EPSILON);
    }
}
