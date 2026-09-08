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

        -- One row per characterization-station session context
        -- message. Readings are joined to a session by timestamp
        -- rather than by a foreign key: the station publishes into
        -- the same `readings` table as the garden, and the session
        -- log says what the bench was holding at the time. See
        -- docs/characterization-station.md for the join.
        CREATE TABLE IF NOT EXISTS charstation_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at TEXT NOT NULL,
            unit_id TEXT NOT NULL,
            batch_label TEXT NOT NULL,
            insertion_number INTEGER,
            notes TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_charstation_sessions_started_at
            ON charstation_sessions (started_at);

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

/// Record one characterization-station session context row. Each
/// message on `charstation/session` is a new row stamped at receipt —
/// the log is append-only, so a mistake is corrected by publishing
/// again rather than by editing history.
pub fn insert_charstation_session(
    conn: &Connection,
    started_at: &str,
    unit_id: &str,
    batch_label: &str,
    insertion_number: Option<i64>,
    notes: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO charstation_sessions
            (started_at, unit_id, batch_label, insertion_number, notes)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![started_at, unit_id, batch_label, insertion_number, notes],
    )?;
    Ok(conn.last_insert_rowid())
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
    fn test_insert_charstation_session() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        let id = insert_charstation_session(
            &conn,
            "2026-09-07T12:00:00Z",
            "3",
            "sand-100gkg",
            Some(4),
            Some("probe reseated"),
        )
        .unwrap();
        assert!(id > 0);

        let (started_at, unit_id, batch, insertion, notes): (
            String,
            String,
            String,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT started_at, unit_id, batch_label, insertion_number, notes
                 FROM charstation_sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(started_at, "2026-09-07T12:00:00Z");
        assert_eq!(unit_id, "3");
        assert_eq!(batch, "sand-100gkg");
        assert_eq!(insertion, Some(4));
        assert_eq!(notes.as_deref(), Some("probe reseated"));
    }

    #[test]
    fn test_charstation_session_optional_fields_may_be_null() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        let id = insert_charstation_session(&conn, "2026-09-07T12:00:00Z", "1", "air", None, None)
            .unwrap();

        let (insertion, notes): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT insertion_number, notes FROM charstation_sessions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(insertion, None);
        assert_eq!(notes, None);
    }

    #[test]
    fn test_charstation_sessions_are_appended_in_order() {
        // The join pattern depends on rows being ordered by
        // started_at: readings between two session rows belong to the
        // earlier one.
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        for (ts, batch) in [
            ("2026-09-07T12:00:00Z", "air"),
            ("2026-09-07T12:10:00Z", "sand-dry"),
            ("2026-09-07T12:20:00Z", "water"),
        ] {
            insert_charstation_session(&conn, ts, "3", batch, None, None).unwrap();
        }

        let mut stmt = conn
            .prepare("SELECT batch_label FROM charstation_sessions ORDER BY started_at")
            .unwrap();
        let batches: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(batches, vec!["air", "sand-dry", "water"]);
    }

    #[test]
    fn test_charstation_session_index_exists() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_charstation_sessions_started_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
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
