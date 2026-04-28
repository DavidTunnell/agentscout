//! Stage 4 — scoring, suppression, and dedup against prior dispositions
//! (SPEC.md §7.2).
//!
//! Scoring formula:
//! - Quantitative tier: `score = frequency × time_saved × ease × tier_weight`
//! - Qualitative tier:  `score = confidence × tier_weight × qualitative_multiplier`
//!
//! Suppression: recommendations with `confidence < confidence_threshold`
//! are flagged `suppressed = true` but kept in the result vec for the
//! "below-threshold" UI per §7.2 Stage 4.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub id: Uuid,
    pub cycle_id: String,
    pub generated_at: i64,
    pub tier_id: String,
    pub name: String,
    pub description: String,
    pub observed_pattern: String,
    pub frequency_per_week: Option<f32>,
    pub est_time_saved_minutes: Option<f32>,
    pub strategic_value: Option<String>,
    /// "low" | "medium" | "high"
    pub build_complexity: String,
    pub confidence: f32,
    /// Indices into `SynthesisInput::clusters` (0-based).
    pub supporting_cluster_indices: Vec<i64>,
    pub starter_scaffold: Option<String>,
    pub score: f32,
    pub suppressed: bool,
    pub disposition: Option<String>,
    pub disposition_note: Option<String>,
    pub disposition_at: Option<i64>,
}

/// Tier-side scoring config drawn from `tier-definitions.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct TierScoringConfig {
    pub id: String,
    pub weight: f32,
    /// "quantitative" | "qualitative"
    pub scoring: String,
    #[serde(default = "default_qualitative_multiplier")]
    pub qualitative_multiplier: f32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_qualitative_multiplier() -> f32 {
    100.0
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ScoringContext<'a> {
    pub tiers: &'a [TierScoringConfig],
    pub confidence_threshold: f32,
}

/// Compute a single recommendation's score given the active tier
/// definitions. Mutates `score` and `suppressed` in place.
pub fn score_recommendation(rec: &mut Recommendation, ctx: &ScoringContext<'_>) {
    let tier = ctx.tiers.iter().find(|t| t.id == rec.tier_id);

    let raw_score = match tier {
        None => 0.0,
        Some(t) if !t.enabled => 0.0,
        Some(t) if t.scoring == "quantitative" => {
            let freq = rec.frequency_per_week.unwrap_or(0.0);
            let time_saved = rec.est_time_saved_minutes.unwrap_or(0.0);
            let ease = ease_factor(&rec.build_complexity);
            freq * time_saved * ease * t.weight
        }
        Some(t) => {
            // qualitative
            rec.confidence * t.weight * t.qualitative_multiplier
        }
    };

    rec.score = raw_score;
    rec.suppressed = rec.confidence < ctx.confidence_threshold;
}

fn ease_factor(complexity: &str) -> f32 {
    match complexity {
        "low" => 1.0,
        "medium" => 0.6,
        "high" => 0.3,
        _ => 0.5,
    }
}

/// Apply scoring to all recommendations, then sort DESC by score.
/// Suppressed recommendations are kept in the vec (for the "below
/// threshold" viewer) but pushed to the bottom.
pub fn rank_recommendations(recs: &mut [Recommendation], ctx: &ScoringContext<'_>) {
    for rec in recs.iter_mut() {
        score_recommendation(rec, ctx);
    }
    recs.sort_by(|a, b| {
        // Suppressed ones go to the bottom regardless of score.
        match (a.suppressed, b.suppressed) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => b
                .score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    });
}

/// Drop recommendations whose name or description is too similar to a
/// prior `not_interested` or `implemented` disposition. Uses a simple
/// token-overlap heuristic — for v1 we lean on the synthesis prompt
/// itself to handle dedup; this is a defensive second pass.
pub fn dedup_against_dispositions(
    recs: Vec<Recommendation>,
    prior: &[PriorDispositionRef],
    similarity_threshold: f32,
) -> Vec<Recommendation> {
    if prior.is_empty() {
        return recs;
    }
    recs.into_iter()
        .filter(|rec| {
            let mut rec_tokens = tokenize(&rec.name);
            rec_tokens.extend(tokenize(&rec.description));
            !prior.iter().any(|p| {
                if !matches!(p.disposition.as_str(), "not_interested" | "implemented") {
                    return false;
                }
                let prior_tokens = tokenize(&p.name);
                let sim = jaccard(&rec_tokens, &prior_tokens);
                sim >= similarity_threshold
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct PriorDispositionRef {
    pub name: String,
    pub disposition: String,
}

fn tokenize(s: &str) -> std::collections::HashSet<String> {
    // Split on any non-alphanumeric run — handles hyphens, slashes,
    // punctuation, etc. uniformly. Keeps tokens of length >= 3 to drop
    // stopword noise without losing meaningful short terms ("API", "UI").
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    intersection / union
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        name: &str,
        tier: &str,
        confidence: f32,
        freq: Option<f32>,
        time_saved: Option<f32>,
    ) -> Recommendation {
        Recommendation {
            id: Uuid::new_v4(),
            cycle_id: "c".into(),
            generated_at: 0,
            tier_id: tier.into(),
            name: name.into(),
            description: "desc".into(),
            observed_pattern: "p".into(),
            frequency_per_week: freq,
            est_time_saved_minutes: time_saved,
            strategic_value: None,
            build_complexity: "low".into(),
            confidence,
            supporting_cluster_indices: vec![0],
            starter_scaffold: None,
            score: 0.0,
            suppressed: false,
            disposition: None,
            disposition_note: None,
            disposition_at: None,
        }
    }

    fn quant_tier(weight: f32) -> TierScoringConfig {
        TierScoringConfig {
            id: "time-reclaimers".into(),
            weight,
            scoring: "quantitative".into(),
            qualitative_multiplier: 100.0,
            enabled: true,
        }
    }

    fn qual_tier(weight: f32) -> TierScoringConfig {
        TierScoringConfig {
            id: "capability-unlocks".into(),
            weight,
            scoring: "qualitative".into(),
            qualitative_multiplier: 100.0,
            enabled: true,
        }
    }

    #[test]
    fn quantitative_score_is_freq_times_time_times_ease_times_weight() {
        let mut r = rec("x", "time-reclaimers", 0.9, Some(5.0), Some(20.0));
        let tiers = [quant_tier(1.0)];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        score_recommendation(&mut r, &ctx);
        // 5 * 20 * 1.0 (low) * 1.0 = 100
        assert!((r.score - 100.0).abs() < 0.01);
        assert!(!r.suppressed);
    }

    #[test]
    fn qualitative_score_uses_confidence_and_multiplier() {
        let mut r = rec("y", "capability-unlocks", 0.8, None, None);
        let tiers = [qual_tier(1.5)];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        score_recommendation(&mut r, &ctx);
        // 0.8 * 1.5 * 100 = 120
        assert!((r.score - 120.0).abs() < 0.01);
    }

    #[test]
    fn suppression_flags_below_threshold() {
        let mut r = rec("z", "time-reclaimers", 0.2, Some(5.0), Some(20.0));
        let tiers = [quant_tier(1.0)];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        score_recommendation(&mut r, &ctx);
        assert!(r.suppressed);
    }

    #[test]
    fn unknown_tier_scores_zero() {
        let mut r = rec("a", "fictional-tier", 0.9, Some(5.0), Some(20.0));
        let tiers = [quant_tier(1.0)];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        score_recommendation(&mut r, &ctx);
        assert_eq!(r.score, 0.0);
    }

    #[test]
    fn disabled_tier_scores_zero() {
        let mut r = rec("a", "time-reclaimers", 0.9, Some(5.0), Some(20.0));
        let tiers = [TierScoringConfig {
            enabled: false,
            ..quant_tier(1.0)
        }];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        score_recommendation(&mut r, &ctx);
        assert_eq!(r.score, 0.0);
    }

    #[test]
    fn ranking_sorts_by_score_descending_with_suppressed_at_bottom() {
        let mut recs = vec![
            rec("low", "time-reclaimers", 0.9, Some(2.0), Some(5.0)), // 10
            rec("high", "time-reclaimers", 0.9, Some(10.0), Some(30.0)), // 300
            rec("supp", "time-reclaimers", 0.1, Some(5.0), Some(20.0)), // suppressed (conf<0.3)
        ];
        let tiers = [quant_tier(1.0)];
        let ctx = ScoringContext {
            tiers: &tiers,
            confidence_threshold: 0.3,
        };
        rank_recommendations(&mut recs, &ctx);
        assert_eq!(recs[0].name, "high");
        assert_eq!(recs[1].name, "low");
        assert_eq!(recs[2].name, "supp");
        assert!(recs[2].suppressed);
    }

    #[test]
    fn dedup_drops_similar_to_not_interested() {
        let recs = vec![
            rec("Auto-write commit messages from diff", "t", 0.9, None, None),
            rec("Refactor auth module to traits", "t", 0.9, None, None),
        ];
        let prior = vec![PriorDispositionRef {
            name: "auto write commit messages".into(),
            disposition: "not_interested".into(),
        }];
        let kept = dedup_against_dispositions(recs, &prior, 0.4);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].name.contains("auth"));
    }

    #[test]
    fn dedup_preserves_dissimilar_recommendations() {
        let recs = vec![
            rec(
                "Generate weekly metrics report from BigQuery",
                "t",
                0.9,
                None,
                None,
            ),
            rec(
                "Triage stale GitHub issues with labels",
                "t",
                0.9,
                None,
                None,
            ),
        ];
        let prior = vec![PriorDispositionRef {
            name: "auto write commit messages".into(),
            disposition: "not_interested".into(),
        }];
        let kept = dedup_against_dispositions(recs, &prior, 0.4);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn dedup_ignores_maybe_later_dispositions() {
        let recs = vec![rec("Auto-write commits", "t", 0.9, None, None)];
        let prior = vec![PriorDispositionRef {
            name: "auto write commits".into(),
            disposition: "maybe_later".into(),
        }];
        let kept = dedup_against_dispositions(recs, &prior, 0.3);
        // Maybe-later doesn't suppress — only not_interested + implemented do
        assert_eq!(kept.len(), 1);
    }
}
