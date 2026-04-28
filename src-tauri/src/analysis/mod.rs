//! Analysis pipeline — clustering, prompt construction, synthesis,
//! scoring, and dedup. See SPEC.md §7 for the high-level stages.

pub mod cluster;
pub mod cost;
pub mod orchestrator;
pub mod prompts;
pub mod scoring;
pub mod synthesize;

pub use cluster::{cluster_captures, Cluster, ClusterConfig};
pub use cost::{
    default_pricing_table, estimate_request_tokens, is_pricing_stale, pricing_table_age_days,
    project_cycle_cost, CostCeilingError, CostEstimate, CycleProjection, ModelPricing,
    PricingTable, ProjectionInput,
};
pub use orchestrator::{run_cycle, CycleResult, OrchestratorDeps};
pub use scoring::{
    dedup_against_dispositions, rank_recommendations, score_recommendation, Recommendation,
};
pub use synthesize::{summarize_clusters, synthesize_recommendations, SynthesisInput};
