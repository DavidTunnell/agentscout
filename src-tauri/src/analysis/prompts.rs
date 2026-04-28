//! Prompt construction for the two analysis stages (SPEC.md §7.2).
//!
//! - **Stage 2 (cluster summarization)**: per-cluster prompt sent to
//!   Sonnet 4.6 (configurable). Captures app + duration + OCR text.
//! - **Stage 3 (cross-cluster synthesis)**: single Opus 4.7 prompt that
//!   includes user-profile, tier-definitions, prior-rec dispositions,
//!   and all cluster summaries; returns ranked recommendations as JSON.

use crate::analysis::cluster::Cluster;
use crate::storage::CaptureRow;
use std::collections::HashMap;

/// System prompt for cluster summarization. Kept tight so the per-cluster
/// call stays under ~500 input tokens of fixed prefix.
pub const CLUSTER_SUMMARY_SYSTEM: &str = r#"You are an analyst summarizing a single work session captured from a user's screen.

Given the application name, window-title context, duration, and OCR-extracted text, produce a 2-3 sentence summary describing:
1. What the user was doing (the activity, not the app)
2. What tools or artifacts appeared
3. Any repetition pattern WITHIN this session (e.g., "user retyped the same SQL three times")

Keep it concrete. Cite specific identifiers (function names, file names, ticket IDs) when present in the text. If the OCR text is sparse or noisy, say so briefly rather than inventing details.

Output format: plain prose. No headings. No preamble. Under 80 words."#;

/// Build the user message for a single cluster summarization call.
///
/// `captures_in_cluster` is the slice of CaptureRow that belong to this
/// cluster (already filtered upstream). Only OCR text is included by
/// default — image bytes are appended by the caller when sending vision
/// content (full-resolution mode).
pub fn cluster_summary_user_message(
    cluster: &Cluster,
    captures_in_cluster: &[&CaptureRow],
) -> String {
    let duration_min = cluster.duration().as_secs() / 60;
    let mut s = String::new();
    s.push_str(&format!("**App signature:** {}\n", cluster.app_signature));
    s.push_str(&format!(
        "**Duration:** {} min ({} captures)\n",
        duration_min, cluster.capture_count
    ));
    s.push_str(&format!(
        "**Time range:** {} → {} (unix seconds)\n\n",
        cluster.start_timestamp, cluster.end_timestamp
    ));

    let titles: Vec<&str> = captures_in_cluster
        .iter()
        .filter_map(|c| c.foreground_window_title.as_deref())
        .collect();
    if !titles.is_empty() {
        s.push_str("**Window titles seen:**\n");
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for t in &titles {
            *counts.entry(*t).or_insert(0) += 1;
        }
        let mut entries: Vec<_> = counts.into_iter().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        for (title, n) in entries.iter().take(8) {
            s.push_str(&format!("- ({}×) {}\n", n, title));
        }
        s.push('\n');
    }

    let ocr_chunks: Vec<&str> = captures_in_cluster
        .iter()
        .filter_map(|c| c.ocr_text.as_deref())
        .filter(|t| !t.trim().is_empty())
        .collect();
    if ocr_chunks.is_empty() {
        s.push_str("**OCR text:** (none — budget mode disabled or OCR engine unavailable)\n");
    } else {
        s.push_str("**OCR text (concatenated, deduped):**\n");
        s.push_str("```\n");
        s.push_str(&dedupe_lines(&ocr_chunks).join("\n"));
        s.push_str("\n```\n");
    }

    s
}

/// System prompt for cross-cluster synthesis. The static prefix here is
/// what the prompt-caching breakpoint targets — same across all
/// recommendations within a cycle.
pub const SYNTHESIS_SYSTEM_PREFIX: &str = r#"You are AgentScout's synthesis engine. You analyze a user's observed work patterns and produce ranked recommendations for AI agents that would meaningfully help them.

You will be given:
1. The user's profile (role, company, goals, constraints)
2. Their tier definitions (rubric for what kinds of opportunities matter to them, with weights)
3. Prior recommendations they've already disposed of (Implemented / Not Interested / Maybe Later)
4. Cluster summaries from this analysis cycle

Your job: produce JSON with the top N agent opportunities. For each, provide:
  - `name`: short, action-oriented
  - `tier_id`: must match one of the user's enabled tiers
  - `description`: 1-2 sentences
  - `observed_pattern`: cite cluster IDs as evidence (e.g., "clusters 4, 7, 12")
  - `frequency_per_week`: float, for quantitative tiers; null for qualitative
  - `est_time_saved_minutes`: float per week, for quantitative tiers; null for qualitative
  - `strategic_value`: short string, for qualitative tiers; null for quantitative
  - `build_complexity`: "low" | "medium" | "high"
  - `confidence`: 0.0–1.0, your confidence this opportunity is real and worth pursuing
  - `supporting_cluster_ids`: array of cluster IDs that drove this recommendation
  - `starter_scaffold`: brief pseudocode showing the agent's structure (Python or TypeScript Agent SDK)

Critical rules:
- Skip opportunities semantically similar to any prior "Not Interested" rec unless there's substantial new evidence
- Skip opportunities the user already implemented unless you have evidence the implementation isn't covering observed need
- Reference only cluster IDs that exist in the input — never invent IDs
- Output valid JSON. No preamble, no markdown fence, just the array."#;

/// Build the dynamic suffix of the synthesis prompt — the per-cycle
/// content that follows the cache breakpoint.
pub fn synthesis_user_message(
    user_profile_md: &str,
    tier_definitions_json: &str,
    prior_dispositions: &[PriorDisposition],
    clusters_with_summaries: &[Cluster],
    top_n: u32,
) -> String {
    let mut s = String::new();
    s.push_str("# User profile\n\n");
    s.push_str(user_profile_md.trim());
    s.push_str("\n\n# Tier definitions\n\n```json\n");
    s.push_str(tier_definitions_json.trim());
    s.push_str("\n```\n\n");

    s.push_str("# Prior recommendation dispositions\n\n");
    if prior_dispositions.is_empty() {
        s.push_str("(none — first analysis cycle)\n\n");
    } else {
        for p in prior_dispositions {
            s.push_str(&format!(
                "- **{}** [{}]: {}\n",
                p.disposition, p.tier_id, p.name
            ));
            if let Some(note) = &p.note {
                s.push_str(&format!("  note: {note}\n"));
            }
        }
        s.push('\n');
    }

    s.push_str("# Cluster summaries from this cycle\n\n");
    for (idx, c) in clusters_with_summaries.iter().enumerate() {
        let summary = c.summary.as_deref().unwrap_or("(no summary)");
        s.push_str(&format!(
            "## Cluster {} — {} ({} min, {} captures)\n{}\n\n",
            idx,
            c.app_signature,
            c.duration().as_secs() / 60,
            c.capture_count,
            summary
        ));
    }

    s.push_str(&format!(
        "# Task\n\nProduce the top {top_n} agent opportunities as a JSON array, ranked by score (highest first). Output only the JSON.\n"
    ));

    s
}

#[derive(Debug, Clone)]
pub struct PriorDisposition {
    pub name: String,
    pub tier_id: String,
    /// "implemented" | "not_interested" | "maybe_later"
    pub disposition: String,
    pub note: Option<String>,
}

/// Drop near-duplicate consecutive lines so OCR jitter doesn't bloat
/// the prompt. Preserves order; case-sensitive comparison.
fn dedupe_lines(chunks: &[&str]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for chunk in chunks {
        for line in chunk.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if seen.insert(t.to_string()) {
                out.push(t.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: i64, ts: i64, app: &str, title: &str, ocr: Option<&str>) -> CaptureRow {
        CaptureRow {
            id,
            timestamp: ts,
            cycle_id: "c".into(),
            foreground_app: Some(app.into()),
            foreground_window_title: Some(title.into()),
            image_path: format!("/tmp/{id}.enc"),
            ocr_text: ocr.map(String::from),
            thumbnail_path: None,
            ocr_engine: ocr.map(|_| "mock".into()),
        }
    }

    fn make_cluster() -> Cluster {
        Cluster {
            cycle_id: "c".into(),
            app_signature: "vscode.exe:project".into(),
            start_timestamp: 1_000,
            end_timestamp: 1_000 + 30 * 60,
            capture_ids: vec![1, 2, 3],
            capture_count: 3,
            summary: None,
        }
    }

    #[test]
    fn cluster_summary_message_includes_metadata_and_titles() {
        let cluster = make_cluster();
        let captures = [
            cap(
                1,
                1_000,
                "vscode.exe",
                "main.rs - project",
                Some("fn main() {"),
            ),
            cap(
                2,
                1_300,
                "vscode.exe",
                "main.rs - project",
                Some("fn main() {"),
            ),
            cap(
                3,
                1_600,
                "vscode.exe",
                "lib.rs - project",
                Some("pub mod foo;"),
            ),
        ];
        let refs: Vec<&CaptureRow> = captures.iter().collect();
        let msg = cluster_summary_user_message(&cluster, &refs);
        assert!(msg.contains("vscode.exe:project"));
        assert!(msg.contains("30 min"));
        assert!(msg.contains("main.rs - project"));
        assert!(msg.contains("fn main() {"));
        assert!(msg.contains("pub mod foo;"));
    }

    #[test]
    fn cluster_summary_handles_missing_ocr() {
        let cluster = make_cluster();
        let captures = [cap(1, 1_000, "vscode.exe", "x - p", None)];
        let refs: Vec<&CaptureRow> = captures.iter().collect();
        let msg = cluster_summary_user_message(&cluster, &refs);
        assert!(msg.contains("budget mode disabled"));
    }

    #[test]
    fn dedupe_lines_drops_repeats() {
        let chunks = vec!["a\nb\na", "b\nc"];
        let out = dedupe_lines(&chunks);
        assert_eq!(out, vec!["a", "b", "c"]);
    }

    #[test]
    fn synthesis_message_includes_all_sections() {
        let cluster = Cluster {
            summary: Some("User refactored auth module".into()),
            ..make_cluster()
        };
        let prior = vec![PriorDisposition {
            name: "Auto-write commit messages".into(),
            tier_id: "time-reclaimers".into(),
            disposition: "not_interested".into(),
            note: Some("already use conventional commits".into()),
        }];
        let msg = synthesis_user_message(
            "**Role:** Engineer",
            "{\"tiers\":[]}",
            &prior,
            &[cluster],
            5,
        );
        assert!(msg.contains("# User profile"));
        assert!(msg.contains("# Tier definitions"));
        assert!(msg.contains("# Prior recommendation dispositions"));
        assert!(msg.contains("Auto-write commit messages"));
        assert!(msg.contains("not_interested"));
        assert!(msg.contains("# Cluster summaries"));
        assert!(msg.contains("User refactored auth module"));
        assert!(msg.contains("top 5 agent opportunities"));
    }

    #[test]
    fn synthesis_message_handles_no_priors() {
        let cluster = make_cluster();
        let msg = synthesis_user_message("profile", "{}", &[], &[cluster], 3);
        assert!(msg.contains("first analysis cycle"));
    }
}
