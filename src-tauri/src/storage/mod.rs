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

    pub fn update_capture_ocr(
        &self,
        capture_id: i64,
        ocr_text: &str,
        ocr_engine: &str,
        thumbnail_path: Option<&str>,
        delete_original: bool,
    ) -> Result<()> {
        self.with_conn(|c| {
            if delete_original {
                c.execute(
                    "UPDATE captures
                     SET ocr_text = ?1, ocr_engine = ?2, thumbnail_path = ?3,
                         image_path = COALESCE(?3, image_path)
                     WHERE id = ?4",
                    rusqlite::params![ocr_text, ocr_engine, thumbnail_path, capture_id],
                )?;
            } else {
                c.execute(
                    "UPDATE captures
                     SET ocr_text = ?1, ocr_engine = ?2, thumbnail_path = ?3
                     WHERE id = ?4",
                    rusqlite::params![ocr_text, ocr_engine, thumbnail_path, capture_id],
                )?;
            }
            Ok(())
        })
    }

    pub fn list_recent_captures(&self, limit: u32) -> Result<Vec<CaptureRow>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, timestamp, cycle_id, foreground_app,
                        foreground_window_title, image_path, ocr_text,
                        thumbnail_path, ocr_engine
                 FROM captures
                 ORDER BY timestamp DESC
                 LIMIT ?1",
            )?;
            let rows = stmt
                .query_map([limit], |row| {
                    Ok(CaptureRow {
                        id: row.get(0)?,
                        timestamp: row.get(1)?,
                        cycle_id: row.get(2)?,
                        foreground_app: row.get(3)?,
                        foreground_window_title: row.get(4)?,
                        image_path: row.get(5)?,
                        ocr_text: row.get(6)?,
                        thumbnail_path: row.get(7)?,
                        ocr_engine: row.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureRow {
    pub id: i64,
    pub timestamp: i64,
    pub cycle_id: String,
    pub foreground_app: Option<String>,
    pub foreground_window_title: Option<String>,
    pub image_path: String,
    pub ocr_text: Option<String>,
    pub thumbnail_path: Option<String>,
    pub ocr_engine: Option<String>,
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
