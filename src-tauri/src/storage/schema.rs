use anyhow::Result;
use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[
    // V1 — initial schema per SPEC.md §6.6
    r#"
    CREATE TABLE captures (
        id INTEGER PRIMARY KEY,
        timestamp INTEGER NOT NULL,
        cycle_id TEXT NOT NULL,
        monitor_ids TEXT NOT NULL,
        foreground_app TEXT,
        foreground_window_title TEXT,
        image_path TEXT NOT NULL,
        ocr_text TEXT,
        thumbnail_path TEXT,
        archived INTEGER DEFAULT 0
    );
    CREATE INDEX idx_captures_cycle ON captures(cycle_id);
    CREATE INDEX idx_captures_timestamp ON captures(timestamp);

    CREATE TABLE clusters (
        id INTEGER PRIMARY KEY,
        cycle_id TEXT NOT NULL,
        app_signature TEXT NOT NULL,
        start_timestamp INTEGER,
        end_timestamp INTEGER,
        capture_count INTEGER,
        summary TEXT
    );
    CREATE INDEX idx_clusters_cycle ON clusters(cycle_id);

    CREATE TABLE capture_cluster_map (
        capture_id INTEGER NOT NULL,
        cluster_id INTEGER NOT NULL,
        PRIMARY KEY (capture_id, cluster_id),
        FOREIGN KEY(capture_id) REFERENCES captures(id) ON DELETE CASCADE,
        FOREIGN KEY(cluster_id) REFERENCES clusters(id) ON DELETE CASCADE
    );

    CREATE TABLE recommendations (
        id TEXT PRIMARY KEY,
        cycle_id TEXT NOT NULL,
        generated_at INTEGER NOT NULL,
        tier_id TEXT NOT NULL,
        name TEXT NOT NULL,
        description TEXT,
        observed_pattern TEXT,
        frequency_per_week REAL,
        est_time_saved_minutes REAL,
        build_complexity TEXT,
        score REAL,
        supporting_cluster_ids TEXT,
        disposition TEXT,
        disposition_note TEXT,
        disposition_at INTEGER,
        suppressed INTEGER DEFAULT 0
    );
    CREATE INDEX idx_recs_cycle ON recommendations(cycle_id);
    CREATE INDEX idx_recs_disposition ON recommendations(disposition);

    CREATE TABLE skip_log (
        timestamp INTEGER PRIMARY KEY,
        reason TEXT NOT NULL
    );

    CREATE TABLE active_hours_counter (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        active_seconds INTEGER NOT NULL DEFAULT 0,
        current_cycle_id TEXT NOT NULL,
        cycle_started_at INTEGER NOT NULL
    );

    CREATE TABLE schema_version (
        version INTEGER PRIMARY KEY
    );
    INSERT INTO schema_version (version) VALUES (1);
    "#,
];

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
        [],
    )?;

    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    for (idx, sql) in MIGRATIONS.iter().enumerate() {
        let target = (idx + 1) as u32;
        if target <= current {
            continue;
        }
        conn.execute_batch(sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_cleanly_to_empty_db() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);

        let table_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name IN
                 ('captures','clusters','capture_cluster_map',
                  'recommendations','skip_log','active_hours_counter')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 6);
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap();
    }
}
