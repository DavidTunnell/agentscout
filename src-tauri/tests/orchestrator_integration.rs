//! End-to-end orchestrator test: seed captures, run the full cycle
//! against mock Anthropic + mock email, assert recommendations got
//! persisted, email was sent, counter reset, and disposition links
//! actually flow through to the storage row.

use agentscout::analysis::{cluster_captures, run_cycle, ClusterConfig, OrchestratorDeps};
use agentscout::anthropic::MockAnthropicClient;
use agentscout::config::Config;
use agentscout::email::{
    start_disposition_server, DispositionAction, DispositionServerConfig, LinkSigner,
    MockEmailSender,
};
use agentscout::storage::{CaptureRecord, CaptureRow, Storage};
use std::sync::Arc;

/// Build a mock with `(n_clusters)` summary responses + one synthesis
/// response. Pre-clusters the captures so the response count matches
/// what the orchestrator will actually call — clustering rules can drop
/// singleton sessions, so naively provisioning N summaries from N seeded
/// captures over-counts.
fn mock_for_captures(captures: &[CaptureRow]) -> MockAnthropicClient {
    let n_clusters = cluster_captures(captures, ClusterConfig::default()).len();
    let mut responses: Vec<String> = (0..n_clusters)
        .map(|i| format!("Cluster {i}: user worked here."))
        .collect();
    responses.push(SYNTHESIS_RESPONSE.to_string());
    MockAnthropicClient::new(responses)
}

fn list_captures(storage: &Storage) -> Vec<CaptureRow> {
    storage.list_recent_captures(1000).unwrap()
}

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
    "observed_pattern": "User reviewed PRs in clusters 0",
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
    let dir = std::env::temp_dir().join(format!("as-orch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = Arc::new(Storage::open_at(dir.clone()).unwrap());
    (storage, dir)
}

fn seed_captures(storage: &Storage, cycle_id: &str, n: u32) {
    for i in 0..n {
        storage
            .record_capture(&CaptureRecord {
                timestamp: 1_700_000_000 + i as i64 * 300,
                cycle_id: cycle_id.to_string(),
                monitor_ids: vec![0],
                foreground_app: Some("Code.exe".into()),
                foreground_window_title: Some("main.rs - my-cli - Visual Studio Code".into()),
                image_path: format!("/tmp/cap-{i}.enc"),
                ocr_text: Some(format!("fn iter_{i}() {{ let x = 42;")),
                thumbnail_path: None,
            })
            .unwrap();
    }
}

fn make_config(recipient: &str) -> Config {
    let mut cfg = Config::default();
    cfg.email.gmail_account = Some(recipient.to_string());
    cfg.email.recipient = Some(recipient.to_string());
    cfg.analysis.confidence_suppression_threshold = 0.3;
    cfg
}

#[tokio::test]
async fn full_cycle_persists_recs_sends_email_and_resets_counter() {
    let (storage, _dir) = temp_storage();
    let active = storage.load_active_hours().unwrap();
    let cycle_id = active.current_cycle_id.clone();

    seed_captures(&storage, &cycle_id, 5);
    let mock_anthropic = mock_for_captures(&list_captures(&storage));

    let mock_email = MockEmailSender::new();
    let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
    let cfg = make_config("user@example.com");

    let deps = OrchestratorDeps {
        config: &cfg,
        storage: storage.clone(),
        anthropic: &mock_anthropic,
        email: &mock_email,
        link_signer: signer.clone(),
        gmail_access_token: "test-token".into(),
        server_origin: "http://127.0.0.1:55555".into(),
        user_profile_md: "**Role:** Test engineer".into(),
        tier_definitions_json: TIER_DEFINITIONS.into(),
    };

    let result = run_cycle(deps).await.expect("cycle runs");
    assert_eq!(result.cycle_id, cycle_id);
    assert!(result.n_recommendations >= 1, "synthesis produced recs");
    assert!(result.email_message_id.is_some(), "email sent");

    // Recommendations persisted in DB
    let count: i64 = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM recommendations WHERE cycle_id = ?1",
                rusqlite::params![cycle_id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(count >= 1);

    // Counter reset, new cycle id
    let after = storage.load_active_hours().unwrap();
    assert_eq!(after.active_seconds, 0);
    assert_ne!(after.current_cycle_id, cycle_id);

    // Email contains the action links via MockEmailSender capture
    let captured = mock_email.last.lock().unwrap().clone().unwrap();
    assert!(captured.html_body.contains("Auto-summarize PRs"));
    assert!(captured
        .html_body
        .contains("http://127.0.0.1:55555/disposition"));
}

#[tokio::test]
async fn empty_cycle_resets_without_calling_anthropic() {
    let (storage, _dir) = temp_storage();
    let mock_anthropic = MockAnthropicClient::new(vec![]); // would error if called
    let mock_email = MockEmailSender::new();
    let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
    let cfg = make_config("user@example.com");

    let deps = OrchestratorDeps {
        config: &cfg,
        storage: storage.clone(),
        anthropic: &mock_anthropic,
        email: &mock_email,
        link_signer: signer.clone(),
        gmail_access_token: "test-token".into(),
        server_origin: "http://127.0.0.1:55555".into(),
        user_profile_md: "**Role:** Test".into(),
        tier_definitions_json: TIER_DEFINITIONS.into(),
    };

    let result = run_cycle(deps).await.expect("empty cycle ok");
    assert_eq!(result.n_captures, 0);
    assert_eq!(result.n_recommendations, 0);
    assert!(
        mock_email.last.lock().unwrap().is_none(),
        "no email on empty"
    );
}

#[tokio::test]
async fn end_to_end_with_real_disposition_server_records_click() {
    // Run a full cycle, then exercise the actual disposition server by
    // hitting one of the URLs from the rendered email and asserting
    // the disposition row updates.
    let (storage, _dir) = temp_storage();
    let active = storage.load_active_hours().unwrap();
    let cycle_id = active.current_cycle_id.clone();
    seed_captures(&storage, &cycle_id, 4);

    let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
    let server = start_disposition_server(
        storage.clone(),
        signer.clone(),
        DispositionServerConfig::default(),
    )
    .await
    .unwrap();

    let mock_anthropic = mock_for_captures(&list_captures(&storage));
    let mock_email = MockEmailSender::new();
    let cfg = make_config("user@example.com");

    let deps = OrchestratorDeps {
        config: &cfg,
        storage: storage.clone(),
        anthropic: &mock_anthropic,
        email: &mock_email,
        link_signer: signer.clone(),
        gmail_access_token: "t".into(),
        server_origin: server.origin.clone(),
        user_profile_md: "**Role:** Test".into(),
        tier_definitions_json: TIER_DEFINITIONS.into(),
    };
    run_cycle(deps).await.unwrap();

    // Pull the rec id we just saved.
    let rec_id: String = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT id FROM recommendations ORDER BY generated_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();

    let url = format!(
        "{}/disposition{}",
        server.origin,
        signer.build_query(&rec_id, DispositionAction::NotInterested)
    );
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let stored: String = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT disposition FROM recommendations WHERE id = ?1",
                rusqlite::params![rec_id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(stored, "not_interested");

    server.shutdown().await;
}
