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
}
