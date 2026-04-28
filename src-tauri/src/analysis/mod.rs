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
    estimate_request_tokens, pricing_table_age_days, CostCeilingError, CostEstimate, ModelPricing,
    PricingTable,
};
pub use orchestrator::{run_cycle, CycleResult, OrchestratorDeps};
pub use scoring::{
    dedup_against_dispositions, rank_recommendations, score_recommendation, Recommendation,
};
pub use synthesize::{summarize_clusters, synthesize_recommendations, SynthesisInput};
