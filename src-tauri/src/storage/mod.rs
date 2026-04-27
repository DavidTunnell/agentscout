pub mod crypto;
pub mod schema;

use crate::config;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Storage {
    conn: Mutex<Connection>,
    root: PathBuf,
}

impl Storage {
    pub fn open() -> Result<Self> {
        let root = config::storage_root()?;
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating storage root at {}", root.display()))?;
        std::fs::create_dir_all(root.join("screenshots"))?;
        std::fs::create_dir_all(root.join("archive"))?;
        std::fs::create_dir_all(root.join("recommendations"))?;

        let db_path = root.join("database.sqlite");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening sqlite at {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        schema::run_migrations(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            root,
        })
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        f(&conn)
    }

    pub fn record_skip(&self, timestamp: i64, reason: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO skip_log (timestamp, reason) VALUES (?1, ?2)",
                rusqlite::params![timestamp, reason],
            )?;
            Ok(())
        })
    }

    pub fn record_capture(&self, capture: &CaptureRecord) -> Result<i64> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO captures (
                    timestamp, cycle_id, monitor_ids, foreground_app,
                    foreground_window_title, image_path, ocr_text,
                    thumbnail_path, archived
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
                rusqlite::params![
                    capture.timestamp,
                    capture.cycle_id,
                    serde_json::to_string(&capture.monitor_ids)?,
                    capture.foreground_app,
                    capture.foreground_window_title,
                    capture.image_path,
                    capture.ocr_text,
                    capture.thumbnail_path,
                ],
            )?;
            Ok(c.last_insert_rowid())
        })
    }
}

#[derive(Debug, Clone)]
pub struct CaptureRecord {
    pub timestamp: i64,
    pub cycle_id: String,
    pub monitor_ids: Vec<u32>,
    pub foreground_app: Option<String>,
    pub foreground_window_title: Option<String>,
    pub image_path: String,
    pub ocr_text: Option<String>,
    pub thumbnail_path: Option<String>,
}
