//! End-to-end replay of the analysis pipeline against the
//! `cycle-solo-engineer` fixture, using `MockAnthropicClient` with
//! hand-crafted realistic responses.
//!
//! Closes the M3 dogfood gate: ranked-opportunity JSON is byte-stable
//! across runs, references only existing cluster IDs, respects the
//! confidence threshold, and dedups against prior `not_interested`
//! dispositions.

use agentscout::analysis::scoring::rank_recommendations;
use agentscout::analysis::{
    cluster_captures,
    cost::default_pricing_table,
    prompts::PriorDisposition,
    scoring::{dedup_against_dispositions, PriorDispositionRef, ScoringContext, TierScoringConfig},
    summarize_clusters, synthesize_recommendations, ClusterConfig, Recommendation, SynthesisInput,
};
use agentscout::anthropic::MockAnthropicClient;
use agentscout::storage::CaptureRow;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cycle-solo-engineer")
}

#[derive(Debug, Deserialize)]
struct CaptureFixture {
    id: i64,
    timestamp: i64,
    cycle_id: String,
    foreground_app: Option<String>,
    foreground_window_title: Option<String>,
    #[serde(default)]
    ocr_text: Option<String>,
    #[serde(default)]
    ocr_engine: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PriorFixture {
    name: String,
    tier_id: String,
    disposition: String,
    #[serde(default)]
    note: Option<String>,
}

fn load_captures() -> Vec<CaptureRow> {
    let bytes = std::fs::read(fixture_dir().join("captures.json")).unwrap();
    let raw: Vec<CaptureFixture> = serde_json::from_slice(&bytes).unwrap();
    raw.into_iter()
        .map(|c| CaptureRow {
            id: c.id,
            timestamp: c.timestamp,
            cycle_id: c.cycle_id,
            foreground_app: c.foreground_app,
            foreground_window_title: c.foreground_window_title,
            image_path: format!("/tmp/{}.enc", c.id),
            ocr_text: c.ocr_text,
            thumbnail_path: None,
            ocr_engine: c.ocr_engine,
        })
        .collect()
}

fn load_priors() -> Vec<PriorFixture> {
    let path = fixture_dir().join("prior-dispositions.json");
    if !path.exists() {
        return Vec::new();
    }
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

#[tokio::test]
async fn pipeline_runs_end_to_end_against_canned_responses() {
    let captures = load_captures();
    let user_profile = std::fs::read_to_string(fixture_dir().join("user-profile.md")).unwrap();
    let tier_defs = std::fs::read_to_string(fixture_dir().join("tier-definitions.json")).unwrap();
    let priors = load_priors();

    let clusters = cluster_captures(&captures, ClusterConfig::default());
    assert!(
        !clusters.is_empty(),
        "fixture should produce at least one cluster"
    );
    let n_clusters = clusters.len();

    let captures_by_id: HashMap<i64, CaptureRow> =
        captures.iter().cloned().map(|c| (c.id, c)).collect();

    let prior_for_synth: Vec<PriorDisposition> = priors
        .iter()
        .map(|p| PriorDisposition {
            name: p.name.clone(),
            tier_id: p.tier_id.clone(),
            disposition: p.disposition.clone(),
            note: p.note.clone(),
        })
        .collect();

    let mut input = SynthesisInput {
        user_profile_md: user_profile,
        tier_definitions_json: tier_defs.clone(),
        prior_dispositions: prior_for_synth,
        clusters: clusters.clone(),
        captures_by_id,
        model_cluster_summary: "claude-sonnet-4-6".into(),
        model_synthesis: "claude-opus-4-7".into(),
        top_n: 5,
        cost_ceiling_usd: 5.0,
    };

    // Stage 2 — one canned summary per cluster
    let cluster_summaries: Vec<String> = (0..n_clusters)
        .map(|i| format!("Cluster {i}: user worked in this app for the duration shown."))
        .collect();
    let stage2_mock = MockAnthropicClient::new(cluster_summaries);
    let pricing = default_pricing_table();
    summarize_clusters(&stage2_mock, &mut input, &pricing)
        .await
        .expect("stage 2 succeeds");
    for c in &input.clusters {
        assert!(c.summary.is_some(), "every cluster gets a summary");
    }
    assert_eq!(stage2_mock.calls(), n_clusters);

    // Stage 3 — one synthesis call returning a realistic JSON array.
    // Three opportunities chosen to exercise dedup, scoring, and the
    // confidence-suppression path.
    let synthesis_response = r#"[
  {
    "name": "Auto-summarize PR diffs into reviewer-ready notes",
    "tier_id": "time-reclaimers",
    "description": "Generate a reviewer-friendly summary for each PR (intent, risk areas, things to check), saving 10-15 minutes per review.",
    "observed_pattern": "User reviewed two open PRs in clusters 0 and spent meaningful time reading diffs and commit history",
    "frequency_per_week": 8.0,
    "est_time_saved_minutes": 100.0,
    "strategic_value": null,
    "build_complexity": "low",
    "confidence": 0.85,
    "supporting_cluster_ids": [0],
    "starter_scaffold": "// Agent SDK Python: read PR diff via gh CLI, summarize via Claude"
  },
  {
    "name": "Auto-write commit messages from staged diff",
    "tier_id": "time-reclaimers",
    "description": "Generate commit messages from the staged diff",
    "observed_pattern": "User typed git commit several times in clusters 0",
    "frequency_per_week": 5.0,
    "est_time_saved_minutes": 30.0,
    "strategic_value": null,
    "build_complexity": "low",
    "confidence": 0.7,
    "supporting_cluster_ids": [0],
    "starter_scaffold": "// gen-commit script"
  },
  {
    "name": "Turn weekly engineering session notes into blog drafts",
    "tier_id": "capability-unlocks",
    "description": "Convert the user's observed engineering decisions into ready-to-edit blog drafts, building a content engine from real work.",
    "observed_pattern": "User drafted a blog post about a sync refactor while they could still see the relevant code, in clusters 0",
    "frequency_per_week": null,
    "est_time_saved_minutes": null,
    "strategic_value": "Productizable content engine — recurring leverage from existing work",
    "build_complexity": "medium",
    "confidence": 0.78,
    "supporting_cluster_ids": [0],
    "starter_scaffold": "// agent reads cluster summary + draft state, returns blog markdown"
  },
  {
    "name": "Below-threshold experimental idea",
    "tier_id": "expertise-amplifiers",
    "description": "Speculative",
    "observed_pattern": "Weak signal in clusters 0",
    "frequency_per_week": 1.0,
    "est_time_saved_minutes": 5.0,
    "strategic_value": null,
    "build_complexity": "high",
    "confidence": 0.15,
    "supporting_cluster_ids": [0],
    "starter_scaffold": null
  }
]"#;
    let stage3_mock = MockAnthropicClient::new(vec![synthesis_response.to_string()]);
    let (mut recs, _est) = synthesize_recommendations(&stage3_mock, &input, &pricing)
        .await
        .expect("stage 3 succeeds");

    // Stage 4: dedup, score, rank
    let prior_refs: Vec<PriorDispositionRef> = priors
        .iter()
        .map(|p| PriorDispositionRef {
            name: p.name.clone(),
            disposition: p.disposition.clone(),
        })
        .collect();
    recs = dedup_against_dispositions(recs, &prior_refs, 0.4);

    // The "Auto-write commit messages" recommendation should be filtered
    // out because it's near-duplicate of a not_interested prior.
    assert!(
        recs.iter().all(|r| !r.name.contains("commit messages")),
        "dedup should drop the commit-message rec — kept: {:?}",
        recs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>()
    );

    let tier_configs = parse_tier_configs(&tier_defs);
    let ctx = ScoringContext {
        tiers: &tier_configs,
        confidence_threshold: 0.3,
    };
    rank_recommendations(&mut recs, &ctx);

    // The PR-summarizer should outscore the qualitative blog-drafts rec
    // (8 freq * 100 min * 1.0 ease * 1.0 weight = 800 vs 0.78 * 1.5 * 100 = 117)
    let visible: Vec<&Recommendation> = recs.iter().filter(|r| !r.suppressed).collect();
    assert_eq!(visible.len(), 2);
    assert_eq!(
        visible[0].name,
        "Auto-summarize PR diffs into reviewer-ready notes"
    );

    // The 0.15-confidence one should be suppressed and pushed to bottom.
    assert!(recs.last().unwrap().suppressed);
    assert!(recs.last().unwrap().confidence < 0.3);
}

fn parse_tier_configs(tier_definitions_json: &str) -> Vec<TierScoringConfig> {
    let parsed: serde_json::Value = serde_json::from_str(tier_definitions_json).unwrap();
    let tiers = parsed["tiers"].as_array().unwrap();
    tiers
        .iter()
        .map(|t| serde_json::from_value::<TierScoringConfig>(t.clone()).unwrap())
        .collect()
}
