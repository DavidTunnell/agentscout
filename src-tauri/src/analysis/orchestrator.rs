//! Cycle orchestrator — runs the full analysis pipeline against the
//! current cycle's captures, persists recommendations, sends the email,
//! and resets state for the next cycle.
//!
//! Wired into the scheduler in week 4: when the active-hours counter
//! exceeds the configured threshold, the scheduler invokes
//! [`run_cycle`] and the user receives an email shortly after.

use crate::analysis::{
    cluster_captures,
    cost::{default_pricing_table, PricingTable},
    prompts::PriorDisposition,
    scoring::{
        dedup_against_dispositions, rank_recommendations, PriorDispositionRef, ScoringContext,
        TierScoringConfig,
    },
    summarize_clusters, synthesize_recommendations, ClusterConfig, Recommendation, SynthesisInput,
};
use crate::anthropic::AnthropicClient;
use crate::config::Config;
use crate::email::{render_email, EmailRenderInput, EmailSender, LinkSigner, RenderedEmail};
use crate::storage::{CaptureRow, Storage};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// One end-to-end run, parameterized by the dependencies it needs. All
/// three trait objects are injected so this function is testable with
/// mocks (no live API, no real email).
pub struct OrchestratorDeps<'a> {
    pub config: &'a Config,
    pub storage: Arc<Storage>,
    pub anthropic: &'a dyn AnthropicClient,
    pub email: &'a dyn EmailSender,
    pub link_signer: Arc<LinkSigner>,
    /// OAuth access token to authorize the Gmail send call. Pass empty
    /// string for the mock email sender.
    pub gmail_access_token: String,
    /// "http://127.0.0.1:<port>" — used to build email action links.
    pub server_origin: String,
    pub user_profile_md: String,
    pub tier_definitions_json: String,
}

#[derive(Debug, Clone)]
pub struct CycleResult {
    pub cycle_id: String,
    pub n_captures: usize,
    pub n_clusters: usize,
    pub n_recommendations: usize,
    pub n_visible: usize,
    pub n_suppressed: usize,
    pub estimated_cost_usd: f64,
    pub email_message_id: Option<String>,
}

pub async fn run_cycle(deps: OrchestratorDeps<'_>) -> Result<CycleResult> {
    let cycle_state = deps.storage.load_active_hours()?;
    let cycle_id = cycle_state.current_cycle_id.clone();
    info!("starting cycle {}", cycle_id);

    let captures = load_cycle_captures(&deps.storage, &cycle_id)?;
    if captures.is_empty() {
        info!("cycle {} has no captures; resetting and exiting", cycle_id);
        deps.storage.reset_active_hours()?;
        return Ok(CycleResult {
            cycle_id,
            n_captures: 0,
            n_clusters: 0,
            n_recommendations: 0,
            n_visible: 0,
            n_suppressed: 0,
            estimated_cost_usd: 0.0,
            email_message_id: None,
        });
    }

    let clusters = cluster_captures(&captures, ClusterConfig::default());
    info!(
        "clustered {} captures into {} clusters",
        captures.len(),
        clusters.len()
    );

    let prior = load_prior_dispositions(&deps.storage)?;
    let pricing = default_pricing_table();

    let captures_by_id: HashMap<i64, CaptureRow> =
        captures.iter().cloned().map(|c| (c.id, c)).collect();

    let mut input = SynthesisInput {
        user_profile_md: deps.user_profile_md.clone(),
        tier_definitions_json: deps.tier_definitions_json.clone(),
        prior_dispositions: prior
            .iter()
            .map(|p| PriorDisposition {
                name: p.name.clone(),
                tier_id: p.tier_id.clone(),
                disposition: p.disposition.clone(),
                note: p.note.clone(),
            })
            .collect(),
        clusters,
        captures_by_id,
        model_cluster_summary: deps.config.analysis.model_cluster_summary.clone(),
        model_synthesis: deps.config.analysis.model_synthesis.clone(),
        top_n: 10,
        cost_ceiling_usd: deps.config.analysis.cost_ceiling_per_cycle_usd as f64,
    };

    let summary_estimates = summarize_clusters(deps.anthropic, &mut input, &pricing)
        .await
        .context("stage 2 (cluster summarization) failed")?;
    let (mut recs, synth_estimate) = synthesize_recommendations(deps.anthropic, &input, &pricing)
        .await
        .context("stage 3 (synthesis) failed")?;
    let total_cost: f64 = summary_estimates
        .iter()
        .map(|e| e.estimated_cost_usd)
        .sum::<f64>()
        + synth_estimate.estimated_cost_usd;

    // Stage 4 — dedup, score, rank, persist.
    let prior_refs: Vec<PriorDispositionRef> = prior
        .iter()
        .map(|p| PriorDispositionRef {
            name: p.name.clone(),
            disposition: p.disposition.clone(),
        })
        .collect();
    recs = dedup_against_dispositions(recs, &prior_refs, 0.4);

    let tier_configs = parse_tier_configs(&deps.tier_definitions_json)?;
    let ctx = ScoringContext {
        tiers: &tier_configs,
        confidence_threshold: deps.config.analysis.confidence_suppression_threshold,
    };
    rank_recommendations(&mut recs, &ctx);

    for rec in &recs {
        if let Err(e) = deps.storage.save_recommendation(rec) {
            warn!("failed to persist recommendation {}: {:#}", rec.id, e);
        }
    }

    let n_visible = recs.iter().filter(|r| !r.suppressed).count();
    let n_suppressed = recs.iter().filter(|r| r.suppressed).count();

    // Render + send the email.
    let rendered = render_for_cycle(
        &recs,
        &input,
        captures.len() as u32,
        total_cost,
        &cycle_id,
        deps.server_origin.clone(),
        deps.link_signer.clone(),
    )?;
    let recipient = deps
        .config
        .email
        .recipient
        .as_deref()
        .or(deps.config.email.gmail_account.as_deref())
        .ok_or_else(|| anyhow!("no email recipient configured"))?;
    let from = deps
        .config
        .email
        .gmail_account
        .as_deref()
        .ok_or_else(|| anyhow!("no Gmail account configured"))?;

    let message_id = match deps
        .email
        .send(&deps.gmail_access_token, from, recipient, &rendered)
        .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            warn!("email send failed: {:#}", e);
            None
        }
    };

    deps.storage.reset_active_hours()?;
    info!(
        "cycle {} done: {} recs ({} visible), cost ${:.4}",
        cycle_id,
        recs.len(),
        n_visible,
        total_cost
    );

    Ok(CycleResult {
        cycle_id,
        n_captures: captures.len(),
        n_clusters: input.clusters.len(),
        n_recommendations: recs.len(),
        n_visible,
        n_suppressed,
        estimated_cost_usd: total_cost,
        email_message_id: message_id,
    })
}

fn load_cycle_captures(storage: &Storage, cycle_id: &str) -> Result<Vec<CaptureRow>> {
    storage.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id, timestamp, cycle_id, foreground_app, foreground_window_title,
                    image_path, ocr_text, thumbnail_path, ocr_engine
             FROM captures
             WHERE cycle_id = ?1 AND archived = 0
             ORDER BY timestamp ASC",
        )?;
        let rows = stmt
            .query_map([cycle_id], |row| {
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

fn load_prior_dispositions(storage: &Storage) -> Result<Vec<crate::storage::PriorDispositionRow>> {
    storage.list_prior_dispositions()
}

fn parse_tier_configs(tier_definitions_json: &str) -> Result<Vec<TierScoringConfig>> {
    let parsed: serde_json::Value = serde_json::from_str(tier_definitions_json)?;
    let tiers = parsed
        .get("tiers")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("tier-definitions.json must have a 'tiers' array"))?;
    tiers
        .iter()
        .map(|t| serde_json::from_value(t.clone()).context("parsing tier entry"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_for_cycle(
    recs: &[Recommendation],
    input: &SynthesisInput,
    n_captures: u32,
    cost: f64,
    cycle_id: &str,
    server_origin: String,
    link_signer: Arc<LinkSigner>,
) -> Result<RenderedEmail> {
    // Active hours = roughly captures × cadence; rough is fine here.
    let active_hours = (n_captures as u64 * 5) / 60;
    let render_input = EmailRenderInput {
        recommendations: recs,
        active_hours: active_hours as u32,
        n_clusters: input.clusters.len(),
        cost_usd: cost,
        cycle_id,
        cycle_date: chrono::Local::now(),
        server_origin,
        link_signer: &link_signer,
    };
    render_email(&render_input)
}

#[allow(unused_imports)]
fn _ensure_pricing_table_referenced(_: PricingTable) {}
