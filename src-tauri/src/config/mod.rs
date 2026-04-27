use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub user_id: Uuid,
    pub capture: CaptureConfig,
    pub blocklist: BlocklistConfig,
    pub analysis: AnalysisConfig,
    pub email: EmailConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub cadence_minutes: u32,
    pub budget_mode: bool,
    pub monitors: Vec<MonitorConfig>,
    pub work_hours: WorkHours,
    pub pause_hotkey: String,
    pub idle_threshold_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    pub id: u32,
    pub enabled: bool,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub days: Vec<String>,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlocklistConfig {
    pub apps: Vec<String>,
    pub window_title_patterns: Vec<String>,
    pub url_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisConfig {
    pub active_hours_threshold: u32,
    pub model_cluster_summary: String,
    pub model_cluster_summary_options: Vec<String>,
    pub model_synthesis: String,
    pub model_synthesis_options: Vec<String>,
    pub perceptual_hash_threshold: f32,
    pub confidence_suppression_threshold: f32,
    pub archive_retention_days: u32,
    pub cost_ceiling_per_cycle_usd: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub gmail_account: Option<String>,
    pub recipient: Option<String>,
    pub send_time_preference: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            user_id: Uuid::new_v4(),
            capture: CaptureConfig::default(),
            blocklist: BlocklistConfig::default(),
            analysis: AnalysisConfig::default(),
            email: EmailConfig::default(),
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            cadence_minutes: 5,
            budget_mode: false,
            monitors: vec![MonitorConfig {
                id: 0,
                enabled: true,
                label: "Primary".into(),
            }],
            work_hours: WorkHours::default(),
            pause_hotkey: "Ctrl+Alt+P".into(),
            idle_threshold_minutes: 5,
        }
    }
}

impl Default for WorkHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "09:00".into(),
            end: "17:00".into(),
            days: vec!["Mon", "Tue", "Wed", "Thu", "Fri"]
                .into_iter()
                .map(Into::into)
                .collect(),
            timezone: "auto".into(),
        }
    }
}

impl Default for BlocklistConfig {
    fn default() -> Self {
        Self {
            apps: vec![
                "1Password.exe".into(),
                "KeePass.exe".into(),
                "Bitwarden.exe".into(),
                "LastPass.exe".into(),
            ],
            window_title_patterns: vec!["*Incognito*".into(), "*Private Browsing*".into()],
            url_domains: vec![],
        }
    }
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            active_hours_threshold: 24,
            model_cluster_summary: "claude-sonnet-4-6".into(),
            model_cluster_summary_options: vec![
                "claude-sonnet-4-6".into(),
                "claude-haiku-4-5".into(),
            ],
            model_synthesis: "claude-opus-4-7".into(),
            model_synthesis_options: vec![
                "claude-opus-4-7".into(),
                "claude-sonnet-4-6".into(),
            ],
            perceptual_hash_threshold: 0.92,
            confidence_suppression_threshold: 0.3,
            archive_retention_days: 30,
            cost_ceiling_per_cycle_usd: 5.0,
        }
    }
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            gmail_account: None,
            recipient: None,
            send_time_preference: "immediate".into(),
        }
    }
}

pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "AgentScout", "AgentScout")
        .context("failed to resolve platform data directory")
}

pub fn storage_root() -> Result<PathBuf> {
    let dirs = project_dirs()?;
    Ok(dirs.data_dir().to_path_buf())
}

pub fn config_path() -> Result<PathBuf> {
    Ok(storage_root()?.join("config.json"))
}

impl Config {
    pub fn load_or_init() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        let version = raw
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let migrated = migrate(raw, version)?;
        let cfg: Self = serde_json::from_value(migrated)?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)
            .with_context(|| format!("writing config to {}", path.display()))?;
        Ok(())
    }
}

fn migrate(mut raw: serde_json::Value, from_version: u32) -> Result<serde_json::Value> {
    if from_version > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "config schema_version {} is newer than supported version {}; \
             downgrade not supported",
            from_version,
            CURRENT_SCHEMA_VERSION
        );
    }
    // No migrations needed yet — version 1 is initial.
    if from_version == 0 {
        raw.as_object_mut()
            .context("config root is not an object")?
            .insert(
                "schema_version".into(),
                serde_json::Value::from(CURRENT_SCHEMA_VERSION),
            );
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips_through_json() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.schema_version, back.schema_version);
        assert_eq!(cfg.capture.cadence_minutes, 5);
        assert_eq!(cfg.analysis.active_hours_threshold, 24);
    }

    #[test]
    fn migration_stamps_schema_version_on_legacy_input() {
        let legacy = serde_json::json!({
            "user_id": "00000000-0000-0000-0000-000000000000",
            "capture": Config::default().capture,
            "blocklist": Config::default().blocklist,
            "analysis": Config::default().analysis,
            "email": Config::default().email,
        });
        let migrated = migrate(legacy, 0).unwrap();
        assert_eq!(
            migrated.get("schema_version").and_then(|v| v.as_u64()),
            Some(CURRENT_SCHEMA_VERSION as u64)
        );
    }
}
