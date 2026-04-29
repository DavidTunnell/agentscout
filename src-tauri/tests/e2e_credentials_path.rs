//! End-to-end credentials → cycle → recommendations integration test.
//!
//! v0.5.5 guardrail: covers the new `analysis-only` path through
//! `run_cycle()` with `email = None`, which is what
//! `cmd_run_cycle_now` will exercise once the user pastes their
//! Anthropic key in the UI.
//!
//! Why this exists: the v0.5.0–v0.5.3 saga happened because we tested
//! library code in isolation and bundle code in isolation, and never
//! end-to-end-tested the path "user does X → backend does Y → user sees
//! Z". This test pins down the contract: given fresh captures and a
//! mock Anthropic client, run_cycle in analysis-only mode produces
//! recommendations in the DB and does NOT panic on a missing Gmail
//! sender.

use agentscout::analysis::{cluster_captures, run_cycle, ClusterConfig, OrchestratorDeps};
use agentscout::anthropic::MockAnthropicClient;
use agentscout::config::Config;
use agentscout::email::LinkSigner;
use agentscout::storage::{CaptureRecord, CaptureRow, Storage};
use std::sync::Arc;

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
    "description": "Generate PR descriptions from diffs",
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

fn temp_storage() -> (Arc<Storage>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("as-creds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = Arc::new(Storage::open_at(dir.clone()).unwrap());
    (storage, dir)
}

fn list_captures(storage: &Storage) -> Vec<CaptureRow> {
    storage.list_recent_captures(1000).unwrap()
}

fn mock_for_captures(captures: &[CaptureRow]) -> MockAnthropicClient {
    let n_clusters = cluster_captures(captures, ClusterConfig::default()).len();
    let mut responses: Vec<String> = (0..n_clusters)
        .map(|i| format!("Cluster {i}: user worked here."))
        .collect();
    responses.push(SYNTHESIS_RESPONSE.to_string());
    MockAnthropicClient::new(responses)
}

fn seed_captures(storage: &Storage, cycle_id: &str, n: u32) {
    for i in 0..n {
        storage
            .record_capture(&CaptureRecord {
                timestamp: 1_700_000_000 + i as i64 * 300,
                cycle_id: cycle_id.to_string(),
                monitor_ids: vec![0],
                foreground_app: Some("Code.exe".into()),
                foreground_window_title: Some("main.rs - my-cli".into()),
                image_path: format!("/tmp/cap-{i}.enc"),
                ocr_text: Some(format!("fn iter_{i}() {{ let x = 42;")),
                thumbnail_path: None,
            })
            .unwrap();
    }
}

#[tokio::test]
async fn analysis_only_cycle_persists_recs_without_email() {
    let (storage, _dir) = temp_storage();
    let active = storage.load_active_hours().unwrap();
    let cycle_id = active.current_cycle_id.clone();

    seed_captures(&storage, &cycle_id, 5);
    let mock_anthropic = mock_for_captures(&list_captures(&storage));

    let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
    let cfg = Config::default();

    // Critical: email = None, gmail_access_token = None.
    // This is what cmd_run_cycle_now passes in v0.5.5.
    let deps = OrchestratorDeps {
        config: &cfg,
        storage: storage.clone(),
        anthropic: &mock_anthropic,
        email: None,
        link_signer: signer.clone(),
        gmail_access_token: None,
        server_origin: "http://127.0.0.1:55555".into(),
        user_profile_md: "**Role:** Test engineer".into(),
        tier_definitions_json: TIER_DEFINITIONS.into(),
    };

    let result = run_cycle(deps).await.expect("analysis-only cycle runs");

    assert_eq!(result.cycle_id, cycle_id);
    assert!(
        result.n_recommendations >= 1,
        "synthesis still produced recs in analysis-only mode"
    );
    assert!(
        result.email_message_id.is_none(),
        "email must NOT have been sent (sender=None)"
    );

    // Recommendations actually persisted in DB
    let count: i64 = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM recommendations WHERE cycle_id = ?1",
                rusqlite::params![cycle_id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(count >= 1, "recs landed in storage");

    // Counter still resets even though email was skipped — required for
    // active-hours-trigger sanity (otherwise the counter would never
    // reset for analysis-only users).
    let after = storage.load_active_hours().unwrap();
    assert_eq!(after.active_seconds, 0);
}

#[tokio::test]
async fn retag_captures_brings_old_cycle_into_current() {
    // Models the cmd_run_cycle_now flow: app started, captures landed
    // under cycle "old", then app was restarted (new cycle "new"),
    // user clicks "Run analysis on last 4 hours". The retag step needs
    // to move those captures into "new" so the orchestrator's
    // WHERE cycle_id = ? query picks them up.
    let (storage, _dir) = temp_storage();

    // Fake a previous cycle with 3 captures.
    seed_captures(&storage, "old-cycle-id", 3);

    // Simulate "app restart" — load_active_hours produces a new cycle.
    let new_cycle = storage.load_active_hours().unwrap().current_cycle_id;
    assert_ne!(new_cycle, "old-cycle-id");

    // Retag captures from before "now" into the new cycle.
    let cutoff = 1_000_000_000; // before any seeded capture
    let n = storage
        .retag_captures_into_cycle(&new_cycle, cutoff)
        .unwrap();
    assert_eq!(n, 3, "all 3 captures retagged");

    // Verify by listing captures.
    let caps = list_captures(&storage);
    assert!(caps.iter().all(|c| c.cycle_id == new_cycle));
}

#[tokio::test]
async fn empty_cycle_still_returns_clean_result_in_analysis_only() {
    let (storage, _dir) = temp_storage();
    let mock_anthropic = MockAnthropicClient::new(vec![]); // would error if called

    let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
    let cfg = Config::default();

    let deps = OrchestratorDeps {
        config: &cfg,
        storage: storage.clone(),
        anthropic: &mock_anthropic,
        email: None,
        link_signer: signer.clone(),
        gmail_access_token: None,
        server_origin: "http://127.0.0.1:55555".into(),
        user_profile_md: "**Role:** Test".into(),
        tier_definitions_json: TIER_DEFINITIONS.into(),
    };

    let result = run_cycle(deps).await.expect("empty cycle ok");
    assert_eq!(result.n_captures, 0);
    assert_eq!(result.n_recommendations, 0);
    assert!(result.email_message_id.is_none());
}
