//! HTML email rendering. Inline CSS only (no external resources, per
//! SPEC.md §8.2 — privacy + deliverability).

use crate::analysis::scoring::Recommendation;
use crate::email::link_signer::{DispositionAction, LinkSigner};
use anyhow::{Context, Result};
use serde::Serialize;
use tera::{Context as TeraContext, Tera};

const EMAIL_TEMPLATE: &str = include_str!("../../templates/email.html.tera");

#[derive(Debug, Serialize)]
struct EmailContext<'a> {
    cycle_date: String,
    n_opportunities: usize,
    active_hours: u32,
    n_clusters: usize,
    total_time_saved_hours: f32,
    cost_usd: f64,
    cost_usd_str: String,
    top_recs: Vec<RecView<'a>>,
    additional_recs: Vec<RecView<'a>>,
    cycle_id: &'a str,
}

#[derive(Debug, Serialize)]
struct RecView<'a> {
    name: &'a str,
    tier_id: &'a str,
    tier_label: String,
    description: &'a str,
    observed_pattern: &'a str,
    is_quantitative: bool,
    frequency_per_week: Option<f32>,
    est_time_saved_minutes: Option<f32>,
    strategic_value: Option<&'a str>,
    build_complexity: &'a str,
    confidence_pct: u32,
    starter_scaffold: Option<&'a str>,
    implemented_url: String,
    not_interested_url: String,
    maybe_later_url: String,
}

/// Inputs for the email render. Structurally minimal — the orchestrator
/// gathers these from storage + config and hands them in.
pub struct EmailRenderInput<'a> {
    pub recommendations: &'a [Recommendation],
    pub active_hours: u32,
    pub n_clusters: usize,
    pub cost_usd: f64,
    pub cycle_id: &'a str,
    pub cycle_date: chrono::DateTime<chrono::Local>,
    pub server_origin: String,
    pub link_signer: &'a LinkSigner,
}

pub struct RenderedEmail {
    pub subject: String,
    pub html_body: String,
    pub plain_body: String,
}

pub fn render(input: &EmailRenderInput<'_>) -> Result<RenderedEmail> {
    let visible: Vec<&Recommendation> = input
        .recommendations
        .iter()
        .filter(|r| !r.suppressed)
        .collect();
    let top_count = 5.min(visible.len());
    let (top, rest) = visible.split_at(top_count);

    let total_time_saved_hours: f32 = visible
        .iter()
        .map(|r| {
            (r.frequency_per_week.unwrap_or(0.0) * r.est_time_saved_minutes.unwrap_or(0.0)) / 60.0
        })
        .sum();

    let top_views: Vec<RecView> = top.iter().map(|r| build_view(r, input)).collect();
    let additional_views: Vec<RecView> = rest.iter().map(|r| build_view(r, input)).collect();

    let ctx = EmailContext {
        cycle_date: input.cycle_date.format("%b %d, %Y").to_string(),
        n_opportunities: visible.len(),
        active_hours: input.active_hours,
        n_clusters: input.n_clusters,
        total_time_saved_hours,
        cost_usd: input.cost_usd,
        cost_usd_str: format!("{:.2}", input.cost_usd),
        top_recs: top_views,
        additional_recs: additional_views,
        cycle_id: input.cycle_id,
    };

    let mut tera = Tera::default();
    tera.add_raw_template("email.html", EMAIL_TEMPLATE)
        .context("registering email template")?;
    let tera_ctx = TeraContext::from_serialize(&ctx).context("building tera context")?;
    let html_body = tera
        .render("email.html", &tera_ctx)
        .context("rendering email template")?;

    let subject = format!(
        "AgentScout — {} new agent opportunities ({})",
        ctx.n_opportunities, ctx.cycle_date
    );
    let plain_body = render_plain(&ctx);

    Ok(RenderedEmail {
        subject,
        html_body,
        plain_body,
    })
}

fn build_view<'a>(r: &'a Recommendation, input: &'a EmailRenderInput<'_>) -> RecView<'a> {
    let id_str = r.id.to_string();
    let implemented_url = format!(
        "{}/disposition{}",
        input.server_origin,
        input
            .link_signer
            .build_query(&id_str, DispositionAction::Implemented)
    );
    let not_interested_url = format!(
        "{}/disposition{}",
        input.server_origin,
        input
            .link_signer
            .build_query(&id_str, DispositionAction::NotInterested)
    );
    let maybe_later_url = format!(
        "{}/disposition{}",
        input.server_origin,
        input
            .link_signer
            .build_query(&id_str, DispositionAction::MaybeLater)
    );

    RecView {
        name: &r.name,
        tier_id: &r.tier_id,
        tier_label: tier_label(&r.tier_id),
        description: &r.description,
        observed_pattern: &r.observed_pattern,
        is_quantitative: r.frequency_per_week.is_some(),
        frequency_per_week: r.frequency_per_week,
        est_time_saved_minutes: r.est_time_saved_minutes,
        strategic_value: r.strategic_value.as_deref(),
        build_complexity: &r.build_complexity,
        confidence_pct: (r.confidence * 100.0).round() as u32,
        starter_scaffold: r.starter_scaffold.as_deref(),
        implemented_url,
        not_interested_url,
        maybe_later_url,
    }
}

fn tier_label(tier_id: &str) -> String {
    match tier_id {
        "time-reclaimers" => "Time Reclaimers".into(),
        "expertise-amplifiers" => "Expertise Amplifiers".into(),
        "capability-unlocks" => "Capability Unlocks".into(),
        other => other.to_string(),
    }
}

fn render_plain(ctx: &EmailContext) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "AgentScout — {} new agent opportunities ({})\n",
        ctx.n_opportunities, ctx.cycle_date
    ));
    s.push_str(&format!(
        "{} active hours observed, {} clusters analyzed.\n",
        ctx.active_hours, ctx.n_clusters
    ));
    s.push_str(&format!(
        "Estimated time saved if all top items implemented: {:.1}h/week.\n",
        ctx.total_time_saved_hours
    ));
    s.push_str(&format!("Cost of this analysis: ${}\n\n", ctx.cost_usd_str));

    s.push_str("Top Recommendations\n-------------------\n\n");
    for (i, r) in ctx.top_recs.iter().enumerate() {
        s.push_str(&format!("{}. {} [{}]\n", i + 1, r.name, r.tier_label));
        s.push_str(&format!("   {}\n", r.description));
        if r.is_quantitative {
            if let (Some(f), Some(t)) = (r.frequency_per_week, r.est_time_saved_minutes) {
                s.push_str(&format!(
                    "   Frequency: {:.1}/week  Saves: ~{:.0} min/week\n",
                    f,
                    f * t
                ));
            }
        } else if let Some(v) = r.strategic_value {
            s.push_str(&format!("   Strategic value: {}\n", v));
        }
        s.push_str(&format!(
            "   Build complexity: {}  Confidence: {}%\n",
            r.build_complexity, r.confidence_pct
        ));
        s.push_str(&format!("   Implemented:    {}\n", r.implemented_url));
        s.push_str(&format!("   Not Interested: {}\n", r.not_interested_url));
        s.push_str(&format!("   Maybe Later:    {}\n\n", r.maybe_later_url));
    }

    if !ctx.additional_recs.is_empty() {
        s.push_str("Additional Recommendations\n--------------------------\n\n");
        for r in &ctx.additional_recs {
            s.push_str(&format!(
                "- {} [{}] (conf {}%)\n",
                r.name, r.tier_label, r.confidence_pct
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn rec(name: &str, tier: &str, conf: f32, freq: Option<f32>) -> Recommendation {
        Recommendation {
            id: Uuid::new_v4(),
            cycle_id: "cycle-1".into(),
            generated_at: 0,
            tier_id: tier.into(),
            name: name.into(),
            description: "desc".into(),
            observed_pattern: "pattern".into(),
            frequency_per_week: freq,
            est_time_saved_minutes: freq.map(|_| 30.0),
            strategic_value: if freq.is_none() {
                Some("strategic".into())
            } else {
                None
            },
            build_complexity: "low".into(),
            confidence: conf,
            supporting_cluster_indices: vec![0],
            starter_scaffold: Some("# scaffold".into()),
            score: 100.0,
            suppressed: false,
            disposition: None,
            disposition_note: None,
            disposition_at: None,
        }
    }

    fn input<'a>(recs: &'a [Recommendation], signer: &'a LinkSigner) -> EmailRenderInput<'a> {
        EmailRenderInput {
            recommendations: recs,
            active_hours: 24,
            n_clusters: 12,
            cost_usd: 1.42,
            cycle_id: "cycle-1",
            cycle_date: chrono::Local
                .with_ymd_and_hms(2026, 4, 27, 12, 0, 0)
                .unwrap(),
            server_origin: "http://127.0.0.1:55555".into(),
            link_signer: signer,
        }
    }

    #[test]
    fn renders_html_subject_and_plain() {
        let recs = vec![
            rec("PR Auto-summarize", "time-reclaimers", 0.9, Some(5.0)),
            rec("Blog drafts", "capability-unlocks", 0.8, None),
        ];
        let signer = LinkSigner::new(vec![7; 32]);
        let r = render(&input(&recs, &signer)).unwrap();
        assert!(r.subject.contains("2 new agent opportunities"));
        assert!(r.html_body.contains("PR Auto-summarize"));
        assert!(r.html_body.contains("Blog drafts"));
        assert!(r.html_body.contains("http://127.0.0.1:55555/disposition"));
        assert!(r.plain_body.contains("PR Auto-summarize"));
    }

    #[test]
    fn suppressed_recommendations_excluded() {
        let mut recs = vec![rec("Visible", "time-reclaimers", 0.9, Some(5.0))];
        let mut hidden = rec("Hidden", "time-reclaimers", 0.1, Some(5.0));
        hidden.suppressed = true;
        recs.push(hidden);

        let signer = LinkSigner::new(vec![7; 32]);
        let r = render(&input(&recs, &signer)).unwrap();
        assert!(r.html_body.contains("Visible"));
        assert!(!r.html_body.contains("Hidden"));
        assert!(r.subject.contains("1 new agent"));
    }

    #[test]
    fn additional_section_appears_for_more_than_five() {
        let recs: Vec<Recommendation> = (0..7)
            .map(|i| rec(&format!("Rec{i}"), "time-reclaimers", 0.9, Some(2.0)))
            .collect();
        let signer = LinkSigner::new(vec![7; 32]);
        let r = render(&input(&recs, &signer)).unwrap();
        assert!(r.html_body.contains("Additional"));
        assert!(r.plain_body.contains("Additional"));
    }

    #[test]
    fn no_recommendations_renders_clean_email() {
        let recs: Vec<Recommendation> = vec![];
        let signer = LinkSigner::new(vec![7; 32]);
        let r = render(&input(&recs, &signer)).unwrap();
        assert!(r.subject.contains("0 new agent"));
        assert!(!r.html_body.contains("Additional"));
    }

    #[test]
    fn links_are_signed_with_provided_signer() {
        let recs = vec![rec("R", "time-reclaimers", 0.9, Some(1.0))];
        let signer = LinkSigner::new(vec![9; 32]);
        let r = render(&input(&recs, &signer)).unwrap();
        // Three actions × 1 rec = 3 hrefs, plus all use the same origin.
        assert_eq!(r.html_body.matches("http://127.0.0.1:55555").count(), 3);
        assert!(r.html_body.contains("action=implemented"));
        assert!(r.html_body.contains("action=not_interested"));
        assert!(r.html_body.contains("action=maybe_later"));
    }
}
