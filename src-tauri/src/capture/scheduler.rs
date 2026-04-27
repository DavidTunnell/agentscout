use super::{
    activity::ActivityMonitor, blocklist::Blocklist, screenshot, screenshot::Screenshotter,
};
use crate::config::{Config, WorkHours};
use crate::ocr::{generate_thumbnail, OcrEngine, ThumbnailFormat};
use crate::storage::{crypto::FileCrypto, CaptureRecord, Storage};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, NaiveTime, Timelike};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub struct Scheduler {
    config: Arc<Mutex<Config>>,
    storage: Arc<Storage>,
    crypto: Arc<FileCrypto>,
    activity: ActivityMonitor,
    ocr_engine: Arc<dyn OcrEngine>,
    screenshotter: Arc<dyn Screenshotter>,
    paused: Arc<AtomicBool>,
    current_cycle_id: Arc<Mutex<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    Captured { capture_id: i64 },
    Skipped { reason: String },
}

impl Scheduler {
    pub fn new(
        config: Arc<Mutex<Config>>,
        storage: Arc<Storage>,
        crypto: Arc<FileCrypto>,
        activity: ActivityMonitor,
        ocr_engine: Arc<dyn OcrEngine>,
        screenshotter: Arc<dyn Screenshotter>,
    ) -> Self {
        Self {
            config,
            storage,
            crypto,
            activity,
            ocr_engine,
            screenshotter,
            paused: Arc::new(AtomicBool::new(false)),
            current_cycle_id: Arc::new(Mutex::new(Uuid::new_v4().to_string())),
        }
    }

    pub fn pause_handle(&self) -> Arc<AtomicBool> {
        self.paused.clone()
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub async fn run(self: Arc<Self>) {
        let cadence = {
            let cfg = self.config.lock().await;
            Duration::from_secs(u64::from(cfg.capture.cadence_minutes) * 60)
        };
        let mut interval = tokio::time::interval(cadence);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        info!("scheduler started with cadence {:?}", cadence);
        loop {
            interval.tick().await;
            match self.tick_once().await {
                Ok(TickOutcome::Captured { capture_id }) => {
                    debug!("capture {} stored", capture_id);
                }
                Ok(TickOutcome::Skipped { reason }) => {
                    debug!("tick skipped: {}", reason);
                }
                Err(e) => {
                    warn!("tick failed: {:#}", e);
                }
            }
        }
    }

    pub async fn tick_once(&self) -> Result<TickOutcome> {
        let now = Local::now();
        let cfg = self.config.lock().await.clone();

        if let Some(reason) = self.gate(&cfg, now) {
            let ts = now.timestamp();
            self.storage.record_skip(ts, &reason)?;
            return Ok(TickOutcome::Skipped { reason });
        }

        let enabled_ids: Vec<u32> = cfg
            .capture
            .monitors
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.id)
            .collect();

        if enabled_ids.is_empty() {
            let reason = "no_monitors_enabled".to_string();
            self.storage.record_skip(now.timestamp(), &reason)?;
            return Ok(TickOutcome::Skipped { reason });
        }

        let captures = self
            .screenshotter
            .capture_enabled(&enabled_ids)
            .context("capturing enabled monitors")?;
        let tiled = screenshot::tile_horizontally(captures).context("tiling captured monitors")?;

        let png = screenshot::encode_png(&tiled)?;
        let encrypted = self.crypto.encrypt(&png)?;

        let cycle_id = self.current_cycle_id.lock().await.clone();
        let stamp = now.format("%Y%m%dT%H%M%S").to_string();
        let filename = format!("{}_tile.enc", stamp);
        let path = self.storage.root().join("screenshots").join(&filename);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &encrypted)
            .with_context(|| format!("writing encrypted capture to {}", path.display()))?;

        let (fg_app, fg_title) = current_foreground();

        let rec = CaptureRecord {
            timestamp: now.timestamp(),
            cycle_id,
            monitor_ids: enabled_ids,
            foreground_app: fg_app,
            foreground_window_title: fg_title,
            image_path: path.to_string_lossy().to_string(),
            ocr_text: None,
            thumbnail_path: None,
        };
        let id = self.storage.record_capture(&rec)?;

        if cfg.capture.budget_mode {
            if let Err(e) = self.run_budget_pipeline(id, &png, &stamp, &path).await {
                warn!("budget-mode pipeline failed for capture {}: {:#}", id, e);
            }
        }

        Ok(TickOutcome::Captured { capture_id: id })
    }

    async fn run_budget_pipeline(
        &self,
        capture_id: i64,
        png: &[u8],
        stamp: &str,
        original_path: &std::path::Path,
    ) -> Result<()> {
        // Thumbnail (encrypted) — written before original is removed so a
        // crash mid-pipeline never leaves us with no visual evidence at all.
        let thumb = generate_thumbnail(png, 400, ThumbnailFormat::WebP)
            .context("generating budget-mode thumbnail")?;
        let enc_thumb = self.crypto.encrypt(&thumb)?;
        let thumb_filename = format!("{}_thumb.enc", stamp);
        let thumb_path = self.storage.root().join("thumbnails").join(&thumb_filename);
        if let Some(parent) = thumb_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&thumb_path, &enc_thumb)
            .with_context(|| format!("writing thumbnail to {}", thumb_path.display()))?;

        // OCR
        let ocr = self
            .ocr_engine
            .extract(png)
            .await
            .context("running OCR on capture")?;
        debug!(
            "OCR extracted {} tokens (engine={}, conf={:.2})",
            ocr.token_count(),
            ocr.engine,
            ocr.confidence
        );

        // Persist results, then delete the original full-res image
        self.storage.update_capture_ocr(
            capture_id,
            &ocr.text,
            &ocr.engine,
            Some(&thumb_path.to_string_lossy()),
            true,
        )?;

        if let Err(e) = std::fs::remove_file(original_path) {
            warn!(
                "failed to remove original after budget-mode pipeline ({}): {}",
                original_path.display(),
                e
            );
        }
        Ok(())
    }

    fn gate(&self, cfg: &Config, now: DateTime<Local>) -> Option<String> {
        if self.paused.load(Ordering::SeqCst) {
            return Some("paused".into());
        }

        let idle_window = Duration::from_secs(u64::from(cfg.capture.idle_threshold_minutes) * 60);
        if !self.activity.is_active_within(idle_window) {
            return Some("idle".into());
        }

        if cfg.capture.work_hours.enabled && !within_work_hours(&cfg.capture.work_hours, now) {
            return Some("outside_work_hours".into());
        }

        let (fg_app, fg_title) = current_foreground();
        let blocklist = Blocklist::from_config(&cfg.blocklist);
        if let Some(reason) = blocklist.is_blocked(fg_app.as_deref(), fg_title.as_deref()) {
            return Some(reason);
        }

        None
    }
}

fn current_foreground() -> (Option<String>, Option<String>) {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => (Some(w.app_name), Some(w.title)),
        Err(_) => (None, None),
    }
}

fn within_work_hours(cfg: &WorkHours, now: DateTime<Local>) -> bool {
    let weekday = now.weekday();
    let weekday_short = match weekday {
        chrono::Weekday::Mon => "Mon",
        chrono::Weekday::Tue => "Tue",
        chrono::Weekday::Wed => "Wed",
        chrono::Weekday::Thu => "Thu",
        chrono::Weekday::Fri => "Fri",
        chrono::Weekday::Sat => "Sat",
        chrono::Weekday::Sun => "Sun",
    };
    if !cfg.days.iter().any(|d| d == weekday_short) {
        return false;
    }
    let Ok(start) = NaiveTime::parse_from_str(&cfg.start, "%H:%M") else {
        return true;
    };
    let Ok(end) = NaiveTime::parse_from_str(&cfg.end, "%H:%M") else {
        return true;
    };
    let cur = NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second()).unwrap_or(start);
    if start <= end {
        cur >= start && cur <= end
    } else {
        cur >= start || cur <= end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn work_hours_within_window() {
        let cfg = WorkHours {
            enabled: true,
            start: "09:00".into(),
            end: "17:00".into(),
            days: vec!["Mon", "Tue", "Wed", "Thu", "Fri"]
                .into_iter()
                .map(String::from)
                .collect(),
            timezone: "auto".into(),
        };
        let tue_noon = Local.with_ymd_and_hms(2026, 4, 21, 12, 0, 0).unwrap();
        assert!(within_work_hours(&cfg, tue_noon));
    }

    #[test]
    fn work_hours_outside_window() {
        let cfg = WorkHours {
            enabled: true,
            start: "09:00".into(),
            end: "17:00".into(),
            days: vec!["Mon"].into_iter().map(String::from).collect(),
            timezone: "auto".into(),
        };
        let sunday_noon = Local.with_ymd_and_hms(2026, 4, 26, 12, 0, 0).unwrap();
        assert!(!within_work_hours(&cfg, sunday_noon));
    }

    #[test]
    fn work_hours_overnight_window() {
        let cfg = WorkHours {
            enabled: true,
            start: "22:00".into(),
            end: "06:00".into(),
            days: vec!["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                .into_iter()
                .map(String::from)
                .collect(),
            timezone: "auto".into(),
        };
        let two_am = Local.with_ymd_and_hms(2026, 4, 21, 2, 0, 0).unwrap();
        let noon = Local.with_ymd_and_hms(2026, 4, 21, 12, 0, 0).unwrap();
        assert!(within_work_hours(&cfg, two_am));
        assert!(!within_work_hours(&cfg, noon));
    }
}
