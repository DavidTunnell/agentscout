//! Synthesis pipeline — Stages 2 + 3 (SPEC.md §7.2).
//!
//! Stage 2: per-cluster summarization via Sonnet 4.6 (configurable).
//! Stage 3: cross-cluster synthesis via Opus 4.7. Returns ranked
//! recommendations as parsed JSON.

use crate::analysis::cluster::Cluster;
use crate::analysis::cost::{self, PricingTable};
use crate::analysis::prompts::{
    cluster_summary_user_message, synthesis_user_message, PriorDisposition, CLUSTER_SUMMARY_SYSTEM,
    SYNTHESIS_SYSTEM_PREFIX,
};
use crate::analysis::scoring::Recommendation;
use crate::anthropic::{AnthropicClient, CompletionRequest, Message, Role};
use crate::storage::CaptureRow;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// Inputs for a full synthesis run. Populated upstream by the
/// orchestrator from storage + config files on disk.
#[derive(Debug, Clone)]
pub struct SynthesisInput {
    pub user_profile_md: String,
    pub tier_definitions_json: String,
    pub prior_dispositions: Vec<PriorDisposition>,
    pub clusters: Vec<Cluster>,
    pub captures_by_id: HashMap<i64, CaptureRow>,
    pub model_cluster_summary: String,
    pub model_synthesis: String,
    pub top_n: u32,
    pub cost_ceiling_usd: f64,
}

/// Stage 2: summarize each cluster individually. Mutates `clusters` in
/// place, populating each `cluster.summary`. Returns per-call cost
/// estimates so the caller can sum them and enforce the ceiling.
pub async fn summarize_clusters(
    client: &dyn AnthropicClient,
    input: &mut SynthesisInput,
    pricing: &PricingTable,
) -> Result<Vec<cost::CostEstimate>> {
    let model = input.model_cluster_summary.clone();
    let mut estimates = Vec::with_capacity(input.clusters.len());

    for cluster in input.clusters.iter_mut() {
        let captures: Vec<&CaptureRow> = cluster
            .capture_ids
            .iter()
            .filter_map(|id| input.captures_by_id.get(id))
            .collect();
        let user_msg = cluster_summary_user_message(cluster, &captures);

        let messages = vec![Message {
            role: Role::User,
            content: user_msg,
        }];
        let req = CompletionRequest {
            messages: &messages,
            system: Some(CLUSTER_SUMMARY_SYSTEM),
            model: &model,
            max_tokens: 256,
            cache_breakpoint: None,
        };

        let estimate = cost::estimate_request(pricing, &req)
            .with_context(|| format!("estimate cluster {} cost", cluster.app_signature))?;

        // Pre-flight ceiling check on running total before each call so a
        // long cycle aborts before consuming budget rather than after.
        let projected_total: f64 = estimates
            .iter()
            .map(|e: &cost::CostEstimate| e.estimated_cost_usd)
            .sum::<f64>()
            + estimate.estimated_cost_usd;
        if projected_total > input.cost_ceiling_usd {
            anyhow::bail!(
                "cost ceiling ${:.2} would be exceeded after cluster {} (projected ${:.4})",
                input.cost_ceiling_usd,
                cluster.app_signature,
                projected_total
            );
        }
        estimates.push(estimate);

        let resp = client
            .complete(req)
            .await
            .with_context(|| format!("summarizing cluster {}", cluster.app_signature))?;
        cluster.summary = Some(resp.text.trim().to_string());
    }
    Ok(estimates)
}

/// Stage 3: cross-cluster synthesis. Sends one Opus call with full
/// context and parses the JSON response into recommendations.
pub async fn synthesize_recommendations(
    client: &dyn AnthropicClient,
    input: &SynthesisInput,
    pricing: &PricingTable,
) -> Result<(Vec<Recommendation>, cost::CostEstimate)> {
    let user_msg = synthesis_user_message(
        &input.user_profile_md,
        &input.tier_definitions_json,
        &input.prior_dispositions,
        &input.clusters,
        input.top_n,
    );

    let messages = vec![Message {
        role: Role::User,
        content: user_msg,
    }];
    let model = input.model_synthesis.clone();
    let req = CompletionRequest {
        messages: &messages,
        system: Some(SYNTHESIS_SYSTEM_PREFIX),
        model: &model,
        max_tokens: 4096,
        // System prompt is the cacheable static prefix; the user message
        // (with cluster summaries) varies per cycle. Cache breakpoint is
        // therefore on message index 0 if there were a "static" message —
        // but our static content lives in `system`, which Anthropic caches
        // automatically when the system prompt is identical across calls.
        cache_breakpoint: None,
    };

    let estimate = cost::estimate_request(pricing, &req).context("estimate synthesis cost")?;

    let resp = client.complete(req).await.context("synthesis API call")?;
    let recs =
        parse_recommendations(&resp.text, input).context("parsing synthesis output as JSON")?;
    Ok((recs, estimate))
}

/// Parse the model's JSON output into typed Recommendations. Validates
/// that every `supporting_cluster_ids` entry refers to an actual cluster
/// in the input — otherwise it's hallucinated and we drop the entry.
fn parse_recommendations(raw: &str, input: &SynthesisInput) -> Result<Vec<Recommendation>> {
    let json_str = strip_markdown_fence(raw);
    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .with_context(|| format!("synthesis output isn't valid JSON: {raw}"))?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("synthesis output must be a JSON array"))?;

    let mut existing_cluster_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for c in &input.clusters {
        for cid in &c.capture_ids {
            existing_cluster_ids.insert(*cid);
        }
    }
    let cluster_indices: std::collections::HashSet<i64> =
        (0..input.clusters.len() as i64).collect();

    let mut recommendations = Vec::with_capacity(arr.len());
    for entry in arr {
        let raw_rec: RawRecommendation = match serde_json::from_value(entry.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("dropping malformed recommendation: {e}");
                continue;
            }
        };
        let supporting: Vec<i64> = raw_rec
            .supporting_cluster_ids
            .into_iter()
            .filter(|id| cluster_indices.contains(id))
            .collect();
        if supporting.is_empty() {
            tracing::warn!(
                "dropping recommendation {} — no valid cluster ids referenced",
                raw_rec.name
            );
            continue;
        }
        recommendations.push(Recommendation {
            id: uuid::Uuid::new_v4(),
            cycle_id: input
                .clusters
                .first()
                .map(|c| c.cycle_id.clone())
                .unwrap_or_default(),
            generated_at: chrono::Utc::now().timestamp(),
            tier_id: raw_rec.tier_id,
            name: raw_rec.name,
            description: raw_rec.description,
            observed_pattern: raw_rec.observed_pattern,
            frequency_per_week: raw_rec.frequency_per_week,
            est_time_saved_minutes: raw_rec.est_time_saved_minutes,
            strategic_value: raw_rec.strategic_value,
            build_complexity: raw_rec.build_complexity,
            confidence: raw_rec.confidence,
            supporting_cluster_indices: supporting,
            starter_scaffold: raw_rec.starter_scaffold,
            score: 0.0, // computed in scoring stage
            suppressed: false,
            disposition: None,
            disposition_note: None,
            disposition_at: None,
        });
    }
    Ok(recommendations)
}

#[derive(Debug, serde::Deserialize)]
struct RawRecommendation {
    name: String,
    tier_id: String,
    description: String,
    observed_pattern: String,
    #[serde(default)]
    frequency_per_week: Option<f32>,
    #[serde(default)]
    est_time_saved_minutes: Option<f32>,
    #[serde(default)]
    strategic_value: Option<String>,
    build_complexity: String,
    confidence: f32,
    supporting_cluster_ids: Vec<i64>,
    #[serde(default)]
    starter_scaffold: Option<String>,
}

fn strip_markdown_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::cost::default_pricing_table;
    use crate::anthropic::MockAnthropicClient;

    fn make_input() -> SynthesisInput {
        SynthesisInput {
            user_profile_md: "**Role:** Solo engineer".into(),
            tier_definitions_json: "{\"tiers\":[]}".into(),
            prior_dispositions: vec![],
            clusters: vec![Cluster {
                cycle_id: "c".into(),
                app_signature: "vscode.exe:project".into(),
                start_timestamp: 1000,
                end_timestamp: 1000 + 30 * 60,
                capture_ids: vec![1, 2],
                capture_count: 2,
                summary: None,
            }],
            captures_by_id: HashMap::new(),
            model_cluster_summary: "claude-sonnet-4-6".into(),
            model_synthesis: "claude-opus-4-7".into(),
            top_n: 3,
            cost_ceiling_usd: 5.0,
        }
    }

    #[tokio::test]
    async fn summarize_clusters_populates_summary_field() {
        let mock = MockAnthropicClient::new(vec!["User edited Rust files for 30 min.".into()]);
        let mut input = make_input();
        let estimates = summarize_clusters(&mock, &mut input, &default_pricing_table())
            .await
            .unwrap();
        assert_eq!(estimates.len(), 1);
        assert_eq!(
            input.clusters[0].summary.as_deref(),
            Some("User edited Rust files for 30 min.")
        );
    }

    #[tokio::test]
    async fn summarize_clusters_aborts_on_cost_ceiling() {
        let mock = MockAnthropicClient::new(vec!["s1".into(), "s2".into()]);
        let mut input = make_input();
        input.cost_ceiling_usd = 0.000001; // absurdly low
        input.clusters.push(Cluster {
            cycle_id: "c".into(),
            app_signature: "chrome.exe:web".into(),
            start_timestamp: 5000,
            end_timestamp: 5000 + 20 * 60,
            capture_ids: vec![3, 4],
            capture_count: 2,
            summary: None,
        });
        let result = summarize_clusters(&mock, &mut input, &default_pricing_table()).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(err_msg.contains("ceiling"));
    }

    #[tokio::test]
    async fn synthesize_parses_valid_json_array() {
        let response = r#"[
            {
                "name": "Auto-summarize PRs",
                "tier_id": "time-reclaimers",
                "description": "Generate PR descriptions from commit history.",
                "observed_pattern": "User wrote 4 PR descriptions in clusters 0",
                "frequency_per_week": 4.0,
                "est_time_saved_minutes": 60.0,
                "strategic_value": null,
                "build_complexity": "low",
                "confidence": 0.9,
                "supporting_cluster_ids": [0],
                "starter_scaffold": "// pseudo scaffold"
            }
        ]"#;
        let mock = MockAnthropicClient::new(vec![response.into()]);
        let input = make_input();
        let (recs, estimate) = synthesize_recommendations(&mock, &input, &default_pricing_table())
            .await
            .unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "Auto-summarize PRs");
        assert_eq!(recs[0].supporting_cluster_indices, vec![0]);
        assert!(estimate.estimated_cost_usd > 0.0);
    }

    #[tokio::test]
    async fn synthesize_drops_recommendations_referencing_unknown_clusters() {
        let response = r#"[
            { "name": "Real", "tier_id": "t1", "description": "d", "observed_pattern": "p",
              "build_complexity": "low", "confidence": 0.8, "supporting_cluster_ids": [0] },
            { "name": "Hallucinated", "tier_id": "t1", "description": "d", "observed_pattern": "p",
              "build_complexity": "low", "confidence": 0.8, "supporting_cluster_ids": [99] }
        ]"#;
        let mock = MockAnthropicClient::new(vec![response.into()]);
        let input = make_input();
        let (recs, _) = synthesize_recommendations(&mock, &input, &default_pricing_table())
            .await
            .unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "Real");
    }

    #[tokio::test]
    async fn synthesize_handles_markdown_fence_around_json() {
        let response = "```json\n[]\n```";
        let mock = MockAnthropicClient::new(vec![response.into()]);
        let input = make_input();
        let (recs, _) = synthesize_recommendations(&mock, &input, &default_pricing_table())
            .await
            .unwrap();
        assert!(recs.is_empty());
    }

    #[tokio::test]
    async fn synthesize_errors_on_invalid_json() {
        let response = "this is not JSON at all";
        let mock = MockAnthropicClient::new(vec![response.into()]);
        let input = make_input();
        let result = synthesize_recommendations(&mock, &input, &default_pricing_table()).await;
        assert!(result.is_err());
    }
}
