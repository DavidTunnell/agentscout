//! Replay-cycle binary — runs the full analysis pipeline against a
//! recorded fixture set. Closes the M3 dogfood gate (SPEC.md §7) by
//! producing byte-stable ranked-opportunity JSON at zero API cost.
//!
//! Usage:
//!     cargo run --bin replay-cycle -- --fixture-dir tests/fixtures/cycle-solo-engineer
//!
//! Fixture directory must contain:
//!   - captures.json        — array of CaptureRow records
//!   - user-profile.md      — the user profile to inject
//!   - tier-definitions.json — tier rubric
//!   - prior-dispositions.json (optional) — prior recs to dedup against
//!   - anthropic/<hash>.json — recorded Anthropic responses
//!
//! Output: ranked recommendations JSON written to stdout (and to
//! `<fixture-dir>/output.json` for diffing in CI).

use agentscout::analysis::scoring::dedup_against_dispositions;
use agentscout::analysis::{
    cluster_captures, cost::default_pricing_table, prompts::PriorDisposition, rank_recommendations,
    scoring::PriorDispositionRef, scoring::ScoringContext, scoring::TierScoringConfig,
    summarize_clusters, synthesize_recommendations, ClusterConfig, SynthesisInput,
};
use agentscout::anthropic::FixtureClient;
use agentscout::storage::CaptureRow;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct ReplayManifest {
    #[serde(default = "default_top_n")]
    top_n: u32,
    #[serde(default = "default_cost_ceiling")]
    cost_ceiling_usd: f64,
    #[serde(default = "default_confidence_threshold")]
    confidence_threshold: f32,
    #[serde(default = "default_dedup_threshold")]
    dedup_similarity_threshold: f32,
    #[serde(default = "default_summary_model")]
    model_cluster_summary: String,
    #[serde(default = "default_synthesis_model")]
    model_synthesis: String,
}

fn default_top_n() -> u32 {
    10
}
fn default_cost_ceiling() -> f64 {
    5.0
}
fn default_confidence_threshold() -> f32 {
    0.3
}
fn default_dedup_threshold() -> f32 {
    0.4
}
fn default_summary_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_synthesis_model() -> String {
    "claude-opus-4-7".into()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fixture_dir = args
        .iter()
        .position(|a| a == "--fixture-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("--fixture-dir <path> required"))?;

    println!("AgentScout replay-cycle");
    println!("  fixture_dir: {}", fixture_dir.display());

    let manifest_path = fixture_dir.join("manifest.json");
    let manifest: ReplayManifest = if manifest_path.exists() {
        serde_json::from_slice(&std::fs::read(&manifest_path)?).context("parsing manifest.json")?
    } else {
        ReplayManifest {
            top_n: default_top_n(),
            cost_ceiling_usd: default_cost_ceiling(),
            confidence_threshold: default_confidence_threshold(),
            dedup_similarity_threshold: default_dedup_threshold(),
            model_cluster_summary: default_summary_model(),
            model_synthesis: default_synthesis_model(),
        }
    };

    let captures = load_captures(&fixture_dir.join("captures.json"))?;
    let user_profile_md = std::fs::read_to_string(fixture_dir.join("user-profile.md"))
        .context("reading user-profile.md")?;
    let tier_definitions_json = std::fs::read_to_string(fixture_dir.join("tier-definitions.json"))
        .context("reading tier-definitions.json")?;
    let prior_dispositions = load_priors(&fixture_dir.join("prior-dispositions.json"))?;
    let pricing = default_pricing_table();

    println!("  captures:    {}", captures.len());
    let mut clusters = cluster_captures(&captures, ClusterConfig::default());
    println!("  clusters:    {}", clusters.len());

    let captures_by_id: HashMap<i64, CaptureRow> =
        captures.iter().cloned().map(|c| (c.id, c)).collect();

    let prior_for_synth: Vec<PriorDisposition> = prior_dispositions
        .iter()
        .map(|p| PriorDisposition {
            name: p.name.clone(),
            tier_id: p.tier_id.clone(),
            disposition: p.disposition.clone(),
            note: p.note.clone(),
        })
        .collect();

    let mut input = SynthesisInput {
        user_profile_md,
        tier_definitions_json: tier_definitions_json.clone(),
        prior_dispositions: prior_for_synth,
        clusters: clusters.clone(),
        captures_by_id,
        model_cluster_summary: manifest.model_cluster_summary,
        model_synthesis: manifest.model_synthesis,
        top_n: manifest.top_n,
        cost_ceiling_usd: manifest.cost_ceiling_usd,
    };

    let client = FixtureClient::new(fixture_dir.join("anthropic"));

    let summary_estimates = summarize_clusters(&client, &mut input, &pricing).await?;
    println!(
        "  stage 2:     summarized {} clusters (est ${:.4})",
        input.clusters.len(),
        summary_estimates
            .iter()
            .map(|e| e.estimated_cost_usd)
            .sum::<f64>()
    );
    // mirror updated cluster summaries onto the local copy used downstream
    clusters = input.clusters.clone();
    let _ = clusters; // silence unused after move

    let (mut recs, synth_estimate) = synthesize_recommendations(&client, &input, &pricing).await?;
    println!(
        "  stage 3:     synthesized {} recommendations (est ${:.4})",
        recs.len(),
        synth_estimate.estimated_cost_usd
    );

    // Stage 4 — score, suppress, dedup, rank.
    let tier_configs = parse_tier_configs(&tier_definitions_json)?;
    let ctx = ScoringContext {
        tiers: &tier_configs,
        confidence_threshold: manifest.confidence_threshold,
    };
    let prior_refs: Vec<PriorDispositionRef> = prior_dispositions
        .iter()
        .map(|p| PriorDispositionRef {
            name: p.name.clone(),
            disposition: p.disposition.clone(),
        })
        .collect();
    recs = dedup_against_dispositions(recs, &prior_refs, manifest.dedup_similarity_threshold);
    rank_recommendations(&mut recs, &ctx);

    let visible: Vec<_> = recs.iter().filter(|r| !r.suppressed).collect();
    let suppressed: Vec<_> = recs.iter().filter(|r| r.suppressed).collect();
    println!(
        "  stage 4:     {} ranked, {} suppressed below confidence {}",
        visible.len(),
        suppressed.len(),
        manifest.confidence_threshold
    );

    let output_path = fixture_dir.join("output.json");
    let output_json = serde_json::to_string_pretty(&recs)?;
    std::fs::write(&output_path, &output_json)?;
    println!("  wrote:       {}", output_path.display());
    println!("---");
    println!("{output_json}");

    println!("\nReplay PASS");
    Ok(())
}

fn load_captures(path: &Path) -> Result<Vec<CaptureRow>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading captures from {}", path.display()))?;
    let raw: Vec<CaptureFixture> =
        serde_json::from_slice(&bytes).context("parsing captures.json")?;
    Ok(raw
        .into_iter()
        .map(|c| CaptureRow {
            id: c.id,
            timestamp: c.timestamp,
            cycle_id: c.cycle_id,
            foreground_app: c.foreground_app,
            foreground_window_title: c.foreground_window_title,
            image_path: c.image_path.unwrap_or_default(),
            ocr_text: c.ocr_text,
            thumbnail_path: c.thumbnail_path,
            ocr_engine: c.ocr_engine,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct CaptureFixture {
    id: i64,
    timestamp: i64,
    cycle_id: String,
    foreground_app: Option<String>,
    foreground_window_title: Option<String>,
    #[serde(default)]
    image_path: Option<String>,
    #[serde(default)]
    ocr_text: Option<String>,
    #[serde(default)]
    thumbnail_path: Option<String>,
    #[serde(default)]
    ocr_engine: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PriorFixture {
    name: String,
    tier_id: String,
    disposition: String,
    #[serde(default)]
    note: Option<String>,
}

fn load_priors(path: &Path) -> Result<Vec<PriorFixture>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)?;
    let raw: Vec<PriorFixture> = serde_json::from_slice(&bytes).context("parsing priors")?;
    Ok(raw)
}

fn parse_tier_configs(tier_definitions_json: &str) -> Result<Vec<TierScoringConfig>> {
    let parsed: serde_json::Value = serde_json::from_str(tier_definitions_json)?;
    let tiers = parsed
        .get("tiers")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow::anyhow!("tier-definitions.json must have a 'tiers' array"))?;
    let mut out = Vec::new();
    for t in tiers {
        let cfg: TierScoringConfig = serde_json::from_value(t.clone())
            .with_context(|| format!("parsing tier entry: {t}"))?;
        out.push(cfg);
    }
    Ok(out)
}
