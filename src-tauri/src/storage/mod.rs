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
        Self::open_at(config::storage_root()?)
    }

    /// Open storage rooted at an explicit directory. Used by integration
    /// tests and the smoke binary; production should call [`Storage::open`].
    pub fn open_at(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating storage root at {}", root.display()))?;
        std::fs::create_dir_all(root.join("screenshots"))?;
        std::fs::create_dir_all(root.join("archive"))?;
        std::fs::create_dir_all(root.join("recommendations"))?;
        std::fs::create_dir_all(root.join("thumbnails"))?;

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

impl Storage {
    pub fn save_recommendation(&self, rec: &crate::analysis::Recommendation) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO recommendations (
                    id, cycle_id, generated_at, tier_id, name, description,
                    observed_pattern, frequency_per_week, est_time_saved_minutes,
                    build_complexity, score, supporting_cluster_ids,
                    disposition, disposition_note, disposition_at,
                    suppressed, strategic_value, confidence, starter_scaffold
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                    ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
                 )",
                rusqlite::params![
                    rec.id.to_string(),
                    rec.cycle_id,
                    rec.generated_at,
                    rec.tier_id,
                    rec.name,
                    rec.description,
                    rec.observed_pattern,
                    rec.frequency_per_week,
                    rec.est_time_saved_minutes,
                    rec.build_complexity,
                    rec.score,
                    serde_json::to_string(&rec.supporting_cluster_indices)?,
                    rec.disposition,
                    rec.disposition_note,
                    rec.disposition_at,
                    rec.suppressed as i64,
                    rec.strategic_value,
                    rec.confidence,
                    rec.starter_scaffold,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_prior_dispositions(&self) -> Result<Vec<PriorDispositionRow>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT name, tier_id, disposition, disposition_note
                 FROM recommendations
                 WHERE disposition IN ('not_interested', 'implemented', 'maybe_later')
                 ORDER BY disposition_at DESC
                 LIMIT 200",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(PriorDispositionRow {
                        name: row.get(0)?,
                        tier_id: row.get(1)?,
                        disposition: row.get(2)?,
                        note: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[derive(Debug, Clone)]
pub struct PriorDispositionRow {
    pub name: String,
    pub tier_id: String,
    pub disposition: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveHoursState {
    pub active_seconds: i64,
    pub current_cycle_id: String,
    pub cycle_started_at: i64,
}

impl Storage {
    /// Read or initialize the active-hours counter row. The counter
    /// table has a single row (id=1) created on first call so we don't
    /// need a separate migration.
    pub fn load_active_hours(&self) -> Result<ActiveHoursState> {
        self.with_conn(|c| {
            let row: Option<(i64, String, i64)> = c
                .query_row(
                    "SELECT active_seconds, current_cycle_id, cycle_started_at
                     FROM active_hours_counter WHERE id = 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            match row {
                Some((sec, cid, started)) => Ok(ActiveHoursState {
                    active_seconds: sec,
                    current_cycle_id: cid,
                    cycle_started_at: started,
                }),
                None => {
                    let cycle_id = uuid::Uuid::new_v4().to_string();
                    let now = chrono::Utc::now().timestamp();
                    c.execute(
                        "INSERT INTO active_hours_counter
                         (id, active_seconds, current_cycle_id, cycle_started_at)
                         VALUES (1, 0, ?1, ?2)",
                        rusqlite::params![cycle_id, now],
                    )?;
                    Ok(ActiveHoursState {
                        active_seconds: 0,
                        current_cycle_id: cycle_id,
                        cycle_started_at: now,
                    })
                }
            }
        })
    }

    pub fn add_active_seconds(&self, seconds: i64) -> Result<ActiveHoursState> {
        let _ = self.load_active_hours()?; // ensure row exists
        self.with_conn(|c| {
            c.execute(
                "UPDATE active_hours_counter
                 SET active_seconds = active_seconds + ?1
                 WHERE id = 1",
                rusqlite::params![seconds],
            )?;
            let row: (i64, String, i64) = c.query_row(
                "SELECT active_seconds, current_cycle_id, cycle_started_at
                 FROM active_hours_counter WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            Ok(ActiveHoursState {
                active_seconds: row.0,
                current_cycle_id: row.1,
                cycle_started_at: row.2,
            })
        })
    }

    /// Reset the counter and start a new cycle ID. Called when a cycle
    /// completes (after analysis + email) or when the user manually
    /// triggers a new cycle from the tray menu.
    pub fn reset_active_hours(&self) -> Result<ActiveHoursState> {
        self.with_conn(|c| {
            let cycle_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().timestamp();
            c.execute(
                "INSERT OR REPLACE INTO active_hours_counter
                 (id, active_seconds, current_cycle_id, cycle_started_at)
                 VALUES (1, 0, ?1, ?2)",
                rusqlite::params![cycle_id, now],
            )?;
            Ok(ActiveHoursState {
                active_seconds: 0,
                current_cycle_id: cycle_id,
                cycle_started_at: now,
            })
        })
    }

    /// List recommendations newest-first, optionally including those
    /// flagged `suppressed = true`. Used by the review UI Tauri command.
    pub fn list_recommendations(
        &self,
        include_suppressed: bool,
        limit: u32,
    ) -> Result<Vec<RecommendationRow>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, cycle_id, generated_at, tier_id, name, description,
                        observed_pattern, frequency_per_week, est_time_saved_minutes,
                        strategic_value, build_complexity, confidence, score,
                        suppressed, disposition, disposition_at
                 FROM recommendations
                 WHERE (?1 OR suppressed = 0)
                 ORDER BY suppressed ASC, score DESC
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![include_suppressed, limit], |row| {
                    Ok(RecommendationRow {
                        id: row.get(0)?,
                        cycle_id: row.get(1)?,
                        generated_at: row.get(2)?,
                        tier_id: row.get(3)?,
                        name: row.get(4)?,
                        description: row.get(5)?,
                        observed_pattern: row.get(6)?,
                        frequency_per_week: row.get(7)?,
                        est_time_saved_minutes: row.get(8)?,
                        strategic_value: row.get(9)?,
                        build_complexity: row.get(10)?,
                        confidence: row.get(11)?,
                        score: row.get(12)?,
                        suppressed: row.get::<_, i64>(13)? != 0,
                        disposition: row.get(14)?,
                        disposition_at: row.get(15)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Persist a disposition for a recommendation. Action must be one of
    /// `implemented` / `not_interested` / `maybe_later`. Returns Err if
    /// the action is invalid or the rec_id doesn't exist.
    pub fn set_disposition(&self, rec_id: &str, action: &str, note: Option<&str>) -> Result<()> {
        if !matches!(action, "implemented" | "not_interested" | "maybe_later") {
            anyhow::bail!("unknown disposition action: {action}");
        }
        let now = chrono::Utc::now().timestamp();
        self.with_conn(|c| {
            let updated = c.execute(
                "UPDATE recommendations
                 SET disposition = ?1, disposition_at = ?2, disposition_note = ?3
                 WHERE id = ?4",
                rusqlite::params![action, now, note, rec_id],
            )?;
            if updated == 0 {
                anyhow::bail!("no recommendation with id {}", rec_id);
            }
            Ok(())
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecommendationRow {
    pub id: String,
    pub cycle_id: String,
    pub generated_at: i64,
    pub tier_id: String,
    pub name: String,
    pub description: Option<String>,
    pub observed_pattern: Option<String>,
    pub frequency_per_week: Option<f32>,
    pub est_time_saved_minutes: Option<f32>,
    pub strategic_value: Option<String>,
    pub build_complexity: Option<String>,
    pub confidence: Option<f32>,
    pub score: Option<f32>,
    pub suppressed: bool,
    pub disposition: Option<String>,
    pub disposition_at: Option<i64>,
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
