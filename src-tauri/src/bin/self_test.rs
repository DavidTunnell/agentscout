//! Self-test binary — exercises every AgentScout subsystem and prints
//! a per-subsystem pass/fail report. Exit code 0 only if every check
//! passes.
//!
//! Used three ways:
//! 1. **Local diagnostic** — when an installed AgentScout misbehaves,
//!    `agentscout-self_test` prints what's wrong without needing logs.
//! 2. **CI release-time bundle smoke** — `release.yml` extracts the
//!    bundled binary and runs `--self-test` on it. Catches the class of
//!    bug where the binary builds but a subsystem is broken inside the
//!    bundle (e.g., tesseract path missing, keychain access denied).
//! 3. **Weekly cron** — `.github/workflows/weekly-self-test.yml`
//!    detects subsystem regressions between releases.
//!
//! Subsystems checked (v0.5.5 scope):
//! - Storage round-trip (open temp DB, write+read a capture row)
//! - Crypto round-trip (encrypt+decrypt a known blob, byte-equal)
//! - Keychain round-trip (set, get, clear an isolated test entry)
//! - Fixture-based run_cycle (full orchestrator with mock client)
//!
//! Subsystems planned for later phases:
//! - v0.5.6: Setup conversation state machine round-trip
//! - v0.5.7: HMAC link sign/verify, OAuth state-machine mock
//! - v0.5.8: Scheduler heartbeat, last-cycle freshness

use agentscout::analysis::{cluster_captures, run_cycle, ClusterConfig, OrchestratorDeps};
use agentscout::anthropic::MockAnthropicClient;
use agentscout::config::Config;
use agentscout::email::LinkSigner;
use agentscout::storage::{crypto::FileCrypto, CaptureRecord, CaptureRow, Storage};
use std::sync::Arc;
use std::time::Instant;

const TIER_DEFINITIONS: &str = r#"{
  "schema_version": 1,
  "tiers": [
    { "id": "time-reclaimers", "name": "Time Reclaimers",
      "description": "tactical", "weight": 1.0, "scoring": "quantitative",
      "qualitative_multiplier": 100.0, "enabled": true,
      "example_shapes": [] }
  ]
}"#;

const SYNTHESIS_RESPONSE: &str = r#"[
  {
    "name": "Auto-summarize PRs",
    "tier_id": "time-reclaimers",
    "description": "Generate PR descriptions",
    "observed_pattern": "User reviewed PRs in cluster 0",
    "frequency_per_week": 5.0,
    "est_time_saved_minutes": 30.0,
    "strategic_value": null,
    "build_complexity": "low",
    "confidence": 0.9,
    "supporting_cluster_ids": [0],
    "starter_scaffold": "// scaffold"
  }
]"#;

struct SubsystemReport {
    name: &'static str,
    passed: bool,
    elapsed_ms: u128,
    detail: String,
}

#[tokio::main]
async fn main() {
    println!("AgentScout self-test");
    println!("====================");
    println!();

    let mut reports: Vec<SubsystemReport> = Vec::new();

    reports.push(check_storage_round_trip().await);
    reports.push(check_crypto_round_trip().await);
    reports.push(check_keychain_round_trip().await);
    reports.push(check_fixture_run_cycle().await);

    println!();
    println!("Summary");
    println!("-------");
    for r in &reports {
        let symbol = if r.passed { "  PASS" } else { "  FAIL" };
        println!(
            "{}  {:<32} {:>5}ms  {}",
            symbol, r.name, r.elapsed_ms, r.detail
        );
    }

    let any_failed = reports.iter().any(|r| !r.passed);
    if any_failed {
        println!();
        println!("RESULT: FAIL — at least one subsystem reported failure.");
        std::process::exit(1);
    }
    println!();
    println!("RESULT: PASS — all subsystems healthy.");
}

async fn check_storage_round_trip() -> SubsystemReport {
    let started = Instant::now();
    let dir = std::env::temp_dir().join(format!("as-selftest-storage-{}", uuid::Uuid::new_v4()));
    let result: Result<String, anyhow::Error> = (|| {
        std::fs::create_dir_all(&dir)?;
        let storage = Storage::open_at(dir.clone())?;
        let active = storage.load_active_hours()?;
        let id = storage.record_capture(&CaptureRecord {
            timestamp: 1_700_000_000,
            cycle_id: active.current_cycle_id.clone(),
            monitor_ids: vec![0],
            foreground_app: Some("Test".into()),
            foreground_window_title: Some("hello".into()),
            image_path: "/tmp/cap-st.enc".into(),
            ocr_text: Some("body text".into()),
            thumbnail_path: None,
        })?;
        let listed: Vec<CaptureRow> = storage.list_recent_captures(10)?;
        if listed.iter().any(|r| r.id == id) {
            Ok(format!("opened db, inserted+read row id={}", id))
        } else {
            anyhow::bail!("inserted capture not found in list");
        }
    })();
    cleanup(&dir);
    SubsystemReport::from_result("storage round-trip", started, result)
}

async fn check_crypto_round_trip() -> SubsystemReport {
    let started = Instant::now();
    let dir = std::env::temp_dir().join(format!("as-selftest-crypto-{}", uuid::Uuid::new_v4()));
    let result: Result<String, anyhow::Error> = (|| {
        std::fs::create_dir_all(&dir)?;
        // Use FileCrypto::with_key so we don't touch the real keychain
        // here — that's the keychain subsystem's job, not crypto's.
        let key: [u8; 32] = [0x42; 32];
        let crypto = FileCrypto::with_key(key);
        let payload = b"the quick brown fox jumps over the lazy dog";
        let path = dir.join("blob.enc");
        crypto.encrypt_to_file(&path, payload)?;
        let decrypted = crypto.decrypt_from_file(&path)?;
        if decrypted == payload {
            Ok(format!(
                "encrypted+decrypted {} bytes byte-equal",
                payload.len()
            ))
        } else {
            anyhow::bail!(
                "decrypted bytes differ from input ({} vs {} bytes)",
                decrypted.len(),
                payload.len()
            );
        }
    })();
    cleanup(&dir);
    SubsystemReport::from_result("crypto round-trip", started, result)
}

async fn check_keychain_round_trip() -> SubsystemReport {
    use keyring::Entry;
    let started = Instant::now();
    let probe_account = format!("self-test-probe-{}", uuid::Uuid::new_v4());
    let probe_value = "ok-canary";

    let result: Result<String, anyhow::Error> = (|| {
        let entry = Entry::new("AgentScout", &probe_account)?;
        // Linux without a session keyring will fail here with
        // PlatformFailure / NoStorageAccess. Return a friendly message.
        match entry.set_password(probe_value) {
            Ok(()) => {}
            Err(e) => {
                anyhow::bail!(
                    "keychain set failed (on Linux this needs a running secret-service \
                     daemon like gnome-keyring or libsecret): {}",
                    e
                );
            }
        }
        let got = entry.get_password()?;
        if got != probe_value {
            anyhow::bail!("read-back mismatch");
        }
        let _ = entry.delete_credential();
        Ok("set+read+delete probe entry".into())
    })();

    SubsystemReport::from_result("keychain round-trip", started, result)
}

async fn check_fixture_run_cycle() -> SubsystemReport {
    let started = Instant::now();
    let dir = std::env::temp_dir().join(format!("as-selftest-cycle-{}", uuid::Uuid::new_v4()));
    let result: Result<String, anyhow::Error> = async {
        std::fs::create_dir_all(&dir)?;
        let storage = Arc::new(Storage::open_at(dir.clone())?);
        let cycle_id = storage.load_active_hours()?.current_cycle_id.clone();

        for i in 0..4u32 {
            storage.record_capture(&CaptureRecord {
                timestamp: 1_700_000_000 + i as i64 * 300,
                cycle_id: cycle_id.clone(),
                monitor_ids: vec![0],
                foreground_app: Some("Code.exe".into()),
                foreground_window_title: Some("main.rs".into()),
                image_path: format!("/tmp/cap-{i}.enc"),
                ocr_text: Some(format!("fn main() {{ /* {i} */ }}")),
                thumbnail_path: None,
            })?;
        }

        let captures = storage.list_recent_captures(100)?;
        let n_clusters = cluster_captures(&captures, ClusterConfig::default()).len();
        let mut responses: Vec<String> = (0..n_clusters)
            .map(|i| format!("Cluster {i} summary"))
            .collect();
        responses.push(SYNTHESIS_RESPONSE.to_string());

        let mock = MockAnthropicClient::new(responses);
        let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
        let cfg = Config::default();

        let deps = OrchestratorDeps {
            config: &cfg,
            storage: storage.clone(),
            anthropic: &mock,
            email: None,
            link_signer: signer.clone(),
            gmail_access_token: None,
            server_origin: "http://127.0.0.1:55555".into(),
            user_profile_md: "**Role:** Test".into(),
            tier_definitions_json: TIER_DEFINITIONS.into(),
        };

        let res = run_cycle(deps).await?;
        if res.n_recommendations >= 1 {
            Ok(format!(
                "ran fixture cycle: {} recs ({} visible)",
                res.n_recommendations, res.n_visible
            ))
        } else {
            anyhow::bail!("fixture cycle produced 0 recs");
        }
    }
    .await;
    cleanup(&dir);
    SubsystemReport::from_result("fixture run_cycle", started, result)
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

impl SubsystemReport {
    fn from_result(
        name: &'static str,
        started: Instant,
        result: Result<String, anyhow::Error>,
    ) -> Self {
        let elapsed_ms = started.elapsed().as_millis();
        match result {
            Ok(detail) => {
                println!("  PASS  {} ({}ms): {}", name, elapsed_ms, detail);
                SubsystemReport {
                    name,
                    passed: true,
                    elapsed_ms,
                    detail,
                }
            }
            Err(e) => {
                let detail = format!("{:#}", e);
                println!("  FAIL  {} ({}ms): {}", name, elapsed_ms, detail);
                SubsystemReport {
                    name,
                    passed: false,
                    elapsed_ms,
                    detail,
                }
            }
        }
    }
}
