//! Capture clustering — Stage 1 of the analysis pipeline (SPEC.md §7.2).
//!
//! Pure-Rust, no API calls. Groups captures into work-session clusters
//! based on:
//!   - Foreground application signature (app + window-title bucket)
//!   - Time contiguity (gap < `max_gap`)
//!   - Hard duration cap (`max_duration`) to keep summarization prompts
//!     small and predictable
//!
//! A 24-active-hour cycle at 5-min cadence produces ~288 captures and
//! typically yields 20-60 clusters depending on context-switch frequency.

use crate::storage::CaptureRow;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct ClusterConfig {
    pub max_gap: Duration,
    pub max_duration: Duration,
    pub min_captures: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            // Per SPEC.md §7.2: "Time contiguity (gap less than 15 minutes)"
            max_gap: Duration::from_secs(15 * 60),
            // "Maximum cluster duration (split long clusters at 90 min)"
            max_duration: Duration::from_secs(90 * 60),
            // Single-capture clusters are noisy (transient app focus); drop them.
            min_captures: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cluster {
    pub cycle_id: String,
    pub app_signature: String,
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub capture_ids: Vec<i64>,
    pub capture_count: usize,
    /// Set after summarization (Stage 2). None during clustering itself.
    pub summary: Option<String>,
}

impl Cluster {
    pub fn duration(&self) -> Duration {
        Duration::from_secs((self.end_timestamp - self.start_timestamp).max(0) as u64)
    }
}

pub fn cluster_captures(captures: &[CaptureRow], config: ClusterConfig) -> Vec<Cluster> {
    if captures.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<&CaptureRow> = captures.iter().collect();
    sorted.sort_by_key(|c| c.timestamp);

    let mut clusters: Vec<Cluster> = Vec::new();
    let mut current: Option<Cluster> = None;

    for cap in sorted {
        let signature = app_signature(cap);

        let split = match &current {
            None => true,
            Some(c) => {
                let signature_changed = c.app_signature != signature;
                let gap_exceeded =
                    (cap.timestamp - c.end_timestamp) as u64 > config.max_gap.as_secs();
                let duration_at_cap =
                    Duration::from_secs((cap.timestamp - c.start_timestamp).max(0) as u64);
                let duration_exceeded = duration_at_cap > config.max_duration;
                signature_changed || gap_exceeded || duration_exceeded
            }
        };

        if split {
            if let Some(c) = current.take() {
                if c.capture_count >= config.min_captures {
                    clusters.push(c);
                }
            }
            current = Some(Cluster {
                cycle_id: cap.cycle_id.clone(),
                app_signature: signature,
                start_timestamp: cap.timestamp,
                end_timestamp: cap.timestamp,
                capture_ids: vec![cap.id],
                capture_count: 1,
                summary: None,
            });
        } else if let Some(c) = current.as_mut() {
            c.end_timestamp = cap.timestamp;
            c.capture_ids.push(cap.id);
            c.capture_count += 1;
        }
    }

    if let Some(c) = current.take() {
        if c.capture_count >= config.min_captures {
            clusters.push(c);
        }
    }

    clusters
}

/// Stable signature for grouping. v1 uses app name + a *bucketed* tail
/// segment of the window title only when the app is one we trust to keep
/// a stable suffix (editors, terminals, design tools). For browsers and
/// other apps where titles change every page, the signature is just the
/// app name — otherwise every chrome tab forms its own singleton cluster
/// and gets dropped by the `min_captures` filter.
pub fn app_signature(cap: &CaptureRow) -> String {
    let app = cap
        .foreground_app
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    if !app_uses_title_bucketing(&app) {
        return app;
    }

    let context = cap
        .foreground_window_title
        .as_deref()
        .map(extract_title_context)
        .filter(|s| !s.is_empty());

    match context {
        Some(c) => format!("{app}:{c}"),
        None => app,
    }
}

/// Apps whose window titles end in a stable suffix (project name, repo,
/// canvas, etc.) that's worth bucketing on. Best-effort; expand as we
/// observe more behaviors. Anything not in this list clusters by app
/// name alone.
fn app_uses_title_bucketing(app: &str) -> bool {
    const STABLE_SUFFIX_APPS: &[&str] = &[
        "code.exe",
        "code",
        "vscode.exe",
        "windsurf.exe",
        "cursor.exe",
        "rustrover.exe",
        "idea.exe",
        "figma.exe",
        "figma",
        "notion.exe",
        "obsidian.exe",
        "slack.exe",
        "wezterm-gui.exe",
        "alacritty.exe",
        "windowsterminal.exe",
        "iterm2",
        "terminal",
    ];
    STABLE_SUFFIX_APPS.contains(&app)
}

/// Pull a stable bucket out of a window title. Splits on common
/// separators and returns the LAST segment (typically the project or app
/// name). Falls back to the full title if no separator matches.
fn extract_title_context(title: &str) -> String {
    const SEPARATORS: &[&str] = &[" — ", " - ", " | ", " :: ", " · ", " – "];
    let trimmed = title.trim();
    for sep in SEPARATORS {
        if let Some(idx) = trimmed.rfind(sep) {
            let candidate = trimmed[idx + sep.len()..].trim();
            if !candidate.is_empty() {
                return candidate.to_lowercase();
            }
        }
    }
    trimmed.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(id: i64, ts: i64, app: &str, title: Option<&str>) -> CaptureRow {
        CaptureRow {
            id,
            timestamp: ts,
            cycle_id: "cycle-1".into(),
            foreground_app: Some(app.into()),
            foreground_window_title: title.map(String::from),
            image_path: format!("/tmp/{id}.enc"),
            ocr_text: None,
            thumbnail_path: None,
            ocr_engine: None,
        }
    }

    #[test]
    fn empty_input_yields_empty_clusters() {
        let clusters = cluster_captures(&[], ClusterConfig::default());
        assert!(clusters.is_empty());
    }

    #[test]
    fn contiguous_same_app_captures_form_one_cluster() {
        let captures: Vec<CaptureRow> = (0..5)
            .map(|i| capture(i, 1000 + i * 300, "vscode.exe", Some("main.rs - project")))
            .collect();
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].capture_count, 5);
        assert_eq!(clusters[0].app_signature, "vscode.exe:project");
    }

    #[test]
    fn app_change_splits_cluster() {
        let captures = vec![
            capture(1, 1000, "vscode.exe", Some("a - project")),
            capture(2, 1300, "vscode.exe", Some("b - project")),
            capture(3, 1600, "chrome.exe", Some("Stack Overflow")),
            capture(4, 1900, "chrome.exe", Some("MDN")),
        ];
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].capture_count, 2);
        assert_eq!(clusters[1].capture_count, 2);
    }

    #[test]
    fn gap_larger_than_threshold_splits_cluster() {
        let captures = vec![
            capture(1, 1_000, "vscode.exe", Some("a - p")),
            capture(2, 1_300, "vscode.exe", Some("a - p")),
            // 30 min gap — exceeds default 15 min
            capture(3, 1_300 + 30 * 60, "vscode.exe", Some("a - p")),
            capture(4, 1_300 + 30 * 60 + 300, "vscode.exe", Some("a - p")),
        ];
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn cluster_splits_at_max_duration_cap() {
        // Build 100 minutes of contiguous captures — exceeds 90 min cap
        let captures: Vec<CaptureRow> = (0..21)
            .map(|i| capture(i, 1000 + i * 5 * 60, "vscode.exe", Some("a - p")))
            .collect();
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert!(
            clusters.len() >= 2,
            "expected at least 2 clusters, got {}",
            clusters.len()
        );
        for c in &clusters {
            assert!(
                c.duration() <= Duration::from_secs(95 * 60),
                "cluster duration {}s exceeded cap (with one-tick slack)",
                c.duration().as_secs()
            );
        }
    }

    #[test]
    fn min_captures_filter_drops_singletons() {
        let captures = vec![
            capture(1, 1000, "appA", Some("x - p")),
            // single appB blip
            capture(2, 1300, "appB", Some("y - p")),
            capture(3, 1600, "appA", Some("x - p")),
            capture(4, 1900, "appA", Some("x - p")),
        ];
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        // appA splits into two clusters (with appB blip dropped between).
        // The appA clusters each have <2 captures, so min_captures filter
        // drops them too. Validate the filter aggressively rejects noise.
        for c in &clusters {
            assert!(c.capture_count >= 2);
            assert_ne!(c.app_signature, "appB:p");
        }
    }

    #[test]
    fn missing_app_uses_unknown_marker() {
        let captures = vec![
            CaptureRow {
                id: 1,
                timestamp: 1000,
                cycle_id: "c".into(),
                foreground_app: None,
                foreground_window_title: None,
                image_path: "/tmp/1.enc".into(),
                ocr_text: None,
                thumbnail_path: None,
                ocr_engine: None,
            },
            CaptureRow {
                id: 2,
                timestamp: 1300,
                cycle_id: "c".into(),
                foreground_app: None,
                foreground_window_title: None,
                image_path: "/tmp/2.enc".into(),
                ocr_text: None,
                thumbnail_path: None,
                ocr_engine: None,
            },
        ];
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].app_signature, "unknown");
    }

    #[test]
    fn extract_title_context_picks_last_segment() {
        assert_eq!(
            extract_title_context("file.rs - my-project - VSCode"),
            "vscode"
        );
        assert_eq!(extract_title_context("Stack Overflow"), "stack overflow");
        assert_eq!(extract_title_context("foo — bar"), "bar");
    }

    #[test]
    fn unsorted_input_still_clusters_correctly() {
        let captures = vec![
            capture(3, 1600, "vscode.exe", Some("a - p")),
            capture(1, 1000, "vscode.exe", Some("a - p")),
            capture(2, 1300, "vscode.exe", Some("a - p")),
        ];
        let clusters = cluster_captures(&captures, ClusterConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].capture_count, 3);
        assert_eq!(clusters[0].start_timestamp, 1000);
        assert_eq!(clusters[0].end_timestamp, 1600);
    }
}
