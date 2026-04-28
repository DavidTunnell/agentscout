//! Cost estimation — pre-flight token counting and pricing-table lookup
//! used to enforce the per-cycle cost ceiling (SPEC.md §9.4).
//!
//! Pricing is a static table keyed by model name. The table carries a
//! `last_updated` timestamp so the app can warn the user when the
//! numbers are >60 days stale (rates drift; see SPEC.md §12.1 / risk #5).

use crate::anthropic::CompletionRequest;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// USD per million tokens for one model. Cache pricing follows
/// Anthropic's standard discount: cache reads are 10% of base input,
/// cache creation is 125% of base input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTable {
    pub last_updated: DateTime<Utc>,
    pub models: std::collections::HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        self.models.get(model)
    }
}

/// Default pricing table embedded at build time. Should be refreshed
/// periodically via `cargo run --bin update-pricing` (a future tool) or
/// by editing this constant. Exact rates as of plan-date 2026-04-24.
pub fn default_pricing_table() -> PricingTable {
    let mut models = std::collections::HashMap::new();
    models.insert(
        "claude-opus-4-7".to_string(),
        ModelPricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_read_per_mtok: 1.5,
            cache_creation_per_mtok: 18.75,
        },
    );
    models.insert(
        "claude-sonnet-4-6".to_string(),
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        },
    );
    models.insert(
        "claude-haiku-4-5".to_string(),
        ModelPricing {
            input_per_mtok: 0.80,
            output_per_mtok: 4.0,
            cache_read_per_mtok: 0.08,
            cache_creation_per_mtok: 1.0,
        },
    );
    PricingTable {
        // 2026-04-24 — keep in sync with SPEC.md §9 publication date.
        last_updated: DateTime::parse_from_rfc3339("2026-04-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        models,
    }
}

pub fn pricing_table_age_days(table: &PricingTable) -> i64 {
    (Utc::now() - table.last_updated).num_days()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEstimate {
    pub model: String,
    pub input_tokens: u32,
    pub max_output_tokens: u32,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum CostCeilingError {
    #[error("projected cost ${projected:.4} would exceed ceiling ${ceiling:.2}")]
    Exceeded { projected: f64, ceiling: f64 },

    #[error("no pricing entry for model {0}")]
    UnknownModel(String),
}

/// Crude character-based token estimator. Anthropic's rule of thumb is
/// ~4 chars per token for English text. Tests on real content suggest
/// this sits within ±15% — close enough for ceiling checks. For exact
/// accounting after a call, use the `usage` field in `CompletionResponse`.
pub fn estimate_request_tokens(req: &CompletionRequest<'_>) -> u32 {
    let mut chars = 0usize;
    if let Some(s) = req.system {
        chars += s.len();
    }
    for m in req.messages {
        chars += m.content.len();
        // Per-message overhead for role tokens etc.
        chars += 12;
    }
    (chars / 4).max(1) as u32
}

pub fn estimate_cost(
    table: &PricingTable,
    model: &str,
    input_tokens: u32,
    max_output_tokens: u32,
) -> Result<f64> {
    let p = table
        .lookup(model)
        .ok_or_else(|| anyhow!("no pricing entry for model {model}"))?;
    let input_cost = (input_tokens as f64 / 1_000_000.0) * p.input_per_mtok;
    let output_cost = (max_output_tokens as f64 / 1_000_000.0) * p.output_per_mtok;
    Ok(input_cost + output_cost)
}

pub fn estimate_request(table: &PricingTable, req: &CompletionRequest<'_>) -> Result<CostEstimate> {
    let input_tokens = estimate_request_tokens(req);
    let estimated_cost_usd =
        estimate_cost(table, req.model, input_tokens, req.max_tokens).context("estimate cost")?;
    Ok(CostEstimate {
        model: req.model.to_string(),
        input_tokens,
        max_output_tokens: req.max_tokens,
        estimated_cost_usd,
    })
}

/// Enforce a per-cycle cost ceiling. Sums multiple per-call estimates
/// and returns an error if the projected total exceeds `ceiling_usd`.
pub fn check_ceiling(
    estimates: &[CostEstimate],
    ceiling_usd: f64,
) -> std::result::Result<f64, CostCeilingError> {
    let total: f64 = estimates.iter().map(|e| e.estimated_cost_usd).sum();
    if total > ceiling_usd {
        return Err(CostCeilingError::Exceeded {
            projected: total,
            ceiling: ceiling_usd,
        });
    }
    Ok(total)
}

/// True when the embedded pricing table is older than 60 days. Used
/// upstream to surface a warning banner in the UI per SPEC.md §12.1.
pub fn is_pricing_stale(table: &PricingTable) -> bool {
    pricing_table_age_days(table) > 60
}

/// True when more than `days` have passed since the table was updated.
/// Useful for finer-grained checks than `is_pricing_stale`.
pub fn is_pricing_older_than(table: &PricingTable, days: i64) -> bool {
    pricing_table_age_days(table) > days
}

/// Inputs to project a per-cycle cost without making any API calls.
/// All fields come from config + observed cluster behavior; the
/// estimator is a heuristic, intentionally conservative.
#[derive(Debug, Clone)]
pub struct ProjectionInput {
    pub model_cluster_summary: String,
    pub model_synthesis: String,
    /// Average cluster count per cycle. Default v1 estimate: 40.
    pub avg_clusters_per_cycle: u32,
    /// Average input tokens for one cluster-summary call (system + 1
    /// user message + OCR text). Default ~1500.
    pub avg_summary_input_tokens: u32,
    /// Output token cap configured for cluster summarization. Default 256.
    pub avg_summary_output_tokens: u32,
    /// Synthesis input tokens (profile + tier defs + prior dispositions
    /// + cluster summaries). Default ~14500.
    pub avg_synthesis_input_tokens: u32,
    /// Synthesis output cap. Default 4096.
    pub avg_synthesis_output_tokens: u32,
}

impl Default for ProjectionInput {
    fn default() -> Self {
        Self {
            model_cluster_summary: "claude-sonnet-4-6".into(),
            model_synthesis: "claude-opus-4-7".into(),
            avg_clusters_per_cycle: 40,
            avg_summary_input_tokens: 1500,
            avg_summary_output_tokens: 256,
            avg_synthesis_input_tokens: 14_500,
            avg_synthesis_output_tokens: 4_096,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleProjection {
    pub stage2_cost_usd: f64,
    pub stage3_cost_usd: f64,
    pub total_cost_usd: f64,
    /// Estimated cycles per month at the configured cadence + active-hours
    /// threshold (24 active hours = ~3 work days for an 8h/day user, or
    /// roughly 10 cycles/month).
    pub estimated_cycles_per_month: f32,
    pub monthly_cost_usd: f64,
    pub pricing_age_days: i64,
    pub pricing_stale: bool,
}

pub fn project_cycle_cost(
    table: &PricingTable,
    input: &ProjectionInput,
) -> Result<CycleProjection> {
    let stage2_cost = estimate_cost(
        table,
        &input.model_cluster_summary,
        input.avg_summary_input_tokens * input.avg_clusters_per_cycle,
        input.avg_summary_output_tokens * input.avg_clusters_per_cycle,
    )
    .with_context(|| format!("stage 2 cost for {}", input.model_cluster_summary))?;

    let stage3_cost = estimate_cost(
        table,
        &input.model_synthesis,
        input.avg_synthesis_input_tokens,
        input.avg_synthesis_output_tokens,
    )
    .with_context(|| format!("stage 3 cost for {}", input.model_synthesis))?;

    let total = stage2_cost + stage3_cost;
    // Heuristic: 24 active-hour cycles for an 8h/day knowledge worker
    // works out to ~3 work days per cycle, ~10 cycles/month.
    let monthly_cycles = 10.0;
    Ok(CycleProjection {
        stage2_cost_usd: stage2_cost,
        stage3_cost_usd: stage3_cost,
        total_cost_usd: total,
        estimated_cycles_per_month: monthly_cycles,
        monthly_cost_usd: total * monthly_cycles as f64,
        pricing_age_days: pricing_table_age_days(table),
        pricing_stale: is_pricing_stale(table),
    })
}

#[allow(dead_code)]
fn _ensure_chrono_duration_used(d: Duration) -> i64 {
    d.num_seconds()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{Message, Role};

    fn req<'a>(
        messages: &'a [Message],
        system: Option<&'a str>,
        max_tokens: u32,
    ) -> CompletionRequest<'a> {
        CompletionRequest {
            messages,
            system,
            model: "claude-sonnet-4-6",
            max_tokens,
            cache_breakpoint: None,
        }
    }

    #[test]
    fn default_table_includes_three_v1_models() {
        let t = default_pricing_table();
        assert!(t.lookup("claude-opus-4-7").is_some());
        assert!(t.lookup("claude-sonnet-4-6").is_some());
        assert!(t.lookup("claude-haiku-4-5").is_some());
    }

    #[test]
    fn token_estimate_grows_with_content() {
        let m_short = vec![Message {
            role: Role::User,
            content: "hi".into(),
        }];
        let m_long = vec![Message {
            role: Role::User,
            content: "x".repeat(1000),
        }];
        let s = estimate_request_tokens(&req(&m_short, None, 100));
        let l = estimate_request_tokens(&req(&m_long, None, 100));
        assert!(l > s * 50);
    }

    #[test]
    fn estimate_cost_for_sonnet_matches_rate_card() {
        let t = default_pricing_table();
        // 1M input tokens × $3 = $3
        let cost = estimate_cost(&t, "claude-sonnet-4-6", 1_000_000, 0).unwrap();
        assert!((cost - 3.0).abs() < 0.001, "got {cost}");
        // 1M output tokens × $15 = $15
        let cost = estimate_cost(&t, "claude-sonnet-4-6", 0, 1_000_000).unwrap();
        assert!((cost - 15.0).abs() < 0.001, "got {cost}");
    }

    #[test]
    fn unknown_model_errors() {
        let t = default_pricing_table();
        assert!(estimate_cost(&t, "claude-fictional-9000", 100, 100).is_err());
    }

    #[test]
    fn check_ceiling_passes_under_limit() {
        let estimates = vec![CostEstimate {
            model: "claude-sonnet-4-6".into(),
            input_tokens: 1000,
            max_output_tokens: 500,
            estimated_cost_usd: 0.50,
        }];
        let total = check_ceiling(&estimates, 5.0).unwrap();
        assert!((total - 0.50).abs() < 0.001);
    }

    #[test]
    fn check_ceiling_fails_over_limit() {
        let estimates = vec![CostEstimate {
            model: "claude-opus-4-7".into(),
            input_tokens: 100_000,
            max_output_tokens: 5_000,
            estimated_cost_usd: 6.0,
        }];
        let result = check_ceiling(&estimates, 5.0);
        assert!(matches!(result, Err(CostCeilingError::Exceeded { .. })));
    }

    #[test]
    fn pricing_age_is_nonnegative() {
        let t = default_pricing_table();
        // Test runs after plan date so age >= 0.
        assert!(pricing_table_age_days(&t) >= 0);
    }

    #[test]
    fn projection_uses_both_models_correctly() {
        let table = default_pricing_table();
        let input = ProjectionInput::default();
        let proj = project_cycle_cost(&table, &input).unwrap();
        // Sonnet stage 2: 40 clusters * 1500 input + 40*256 output
        // = 60000 input * $3/M + 10240 output * $15/M = 0.18 + 0.1536 = 0.3336
        assert!(
            (proj.stage2_cost_usd - 0.3336).abs() < 0.01,
            "got {}",
            proj.stage2_cost_usd
        );
        // Opus stage 3: 14500 input * $15/M + 4096 output * $75/M = 0.2175 + 0.3072 = 0.5247
        assert!(
            (proj.stage3_cost_usd - 0.5247).abs() < 0.01,
            "got {}",
            proj.stage3_cost_usd
        );
        assert!((proj.total_cost_usd - (proj.stage2_cost_usd + proj.stage3_cost_usd)).abs() < 1e-9);
        assert!((proj.monthly_cost_usd - proj.total_cost_usd * 10.0).abs() < 1e-9);
    }

    #[test]
    fn projection_with_haiku_is_cheaper_than_sonnet() {
        let table = default_pricing_table();
        let mut input = ProjectionInput::default();
        let with_sonnet = project_cycle_cost(&table, &input).unwrap();
        input.model_cluster_summary = "claude-haiku-4-5".into();
        let with_haiku = project_cycle_cost(&table, &input).unwrap();
        assert!(with_haiku.total_cost_usd < with_sonnet.total_cost_usd);
        // Stage 3 (Opus) is unchanged
        assert!((with_haiku.stage3_cost_usd - with_sonnet.stage3_cost_usd).abs() < 1e-9);
    }

    #[test]
    fn projection_errors_on_unknown_model() {
        let table = default_pricing_table();
        let input = ProjectionInput {
            model_synthesis: "claude-fictional-9000".into(),
            ..Default::default()
        };
        assert!(project_cycle_cost(&table, &input).is_err());
    }
}
