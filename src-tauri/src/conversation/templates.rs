use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarterTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Pre-filled `user-profile.md` skeleton refined by the setup conversation.
    pub profile_skeleton: &'static str,
    /// Pre-filled tier-definitions JSON refined by the tier calibration flow.
    pub tier_skeleton_json: &'static str,
}

pub const STARTER_TEMPLATES: &[StarterTemplate] = &[
    StarterTemplate {
        id: "solo-engineer",
        name: "Solo software engineer / IC",
        description: "Personal productivity, code workflows, tool friction.",
        profile_skeleton: SOLO_ENGINEER_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "engineering-lead",
        name: "Engineering lead / architect",
        description: "Team leverage, knowledge capture, expertise amplification.",
        profile_skeleton: ENGINEERING_LEAD_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "agency-consultant",
        name: "Agency / services consultant",
        description: "Client delivery workflows, productization, multi-client juggling.",
        profile_skeleton: AGENCY_CONSULTANT_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "founder",
        name: "Founder / early-stage operator",
        description: "Growth constraints, capability gaps, scaling bottlenecks.",
        profile_skeleton: FOUNDER_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "ops-sre",
        name: "Ops / SRE / sysadmin",
        description: "Incident response, diagnostic patterns, runbook codification.",
        profile_skeleton: OPS_SRE_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "designer",
        name: "Designer / creative",
        description: "Research synthesis, asset workflows, feedback cycles.",
        profile_skeleton: DESIGNER_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "sales-bd",
        name: "Sales / BD",
        description: "Outreach patterns, research automation, pipeline hygiene.",
        profile_skeleton: SALES_BD_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
    StarterTemplate {
        id: "custom",
        name: "Custom / Blank slate",
        description: "Pure conversational setup, no pre-fills.",
        profile_skeleton: CUSTOM_PROFILE,
        tier_skeleton_json: DEFAULT_TIERS,
    },
];

pub fn find_template(id: &str) -> Option<&'static StarterTemplate> {
    STARTER_TEMPLATES.iter().find(|t| t.id == id)
}

const SOLO_ENGINEER_PROFILE: &str = r#"# User Profile

**Role:** Solo software engineer / IC
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** Solo or small
**Stage:** <fill in>

## What I Do
Day-to-day coding, debugging, and shipping. Tool-stack juggling. Writing
specs, PRs, and docs.

## What I'm Trying to Grow or Achieve
<fill in — e.g., ship faster, reduce context-switch cost, learn a new domain>

## Constraints
<confidentiality, regulated content, things I don't want captured>

## Tech Stack
<editor, languages, frameworks, infra>
"#;

const ENGINEERING_LEAD_PROFILE: &str = r#"# User Profile

**Role:** Engineering lead / architect
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** <fill in>

## What I Do
1:1s, design reviews, code review, planning, removing blockers, hiring.
Some hands-on coding.

## What I'm Trying to Grow or Achieve
<fill in — e.g., team velocity, knowledge sharing, on-call sustainability>

## Constraints
<confidentiality, hiring confidentiality, regulated content>

## Tech Stack
<primary tools and platforms>
"#;

const AGENCY_CONSULTANT_PROFILE: &str = r#"# User Profile

**Role:** Agency / services consultant
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** Services

## What I Do
Client delivery across multiple accounts. Discovery, scoping, building,
QA. Shifting between client contexts and tools daily.

## What I'm Trying to Grow or Achieve
<fill in — e.g., productize a service, reduce per-client setup cost,
increase margins on recurring engagements>

## Constraints
<client NDAs, regulated client content, multi-tenant tooling concerns>

## Tech Stack
<your primary delivery toolchain>
"#;

const FOUNDER_PROFILE: &str = r#"# User Profile

**Role:** Founder / early-stage operator
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** Pre-seed / seed / growth

## What I Do
Sales, hiring, product, fundraising, ops, customer support. Wear many
hats. Constantly context-switching.

## What I'm Trying to Grow or Achieve
<fill in — e.g., revenue growth, hire X role, validate Y market>

## Constraints
<investor confidentiality, regulated industry, customer NDAs>

## Tech Stack
<CRM, productivity, comms, dev tooling>
"#;

const OPS_SRE_PROFILE: &str = r#"# User Profile

**Role:** Ops / SRE / sysadmin
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** <fill in>

## What I Do
On-call rotations, incident response, infrastructure changes, monitoring,
post-mortems, runbook maintenance.

## What I'm Trying to Grow or Achieve
<fill in — e.g., reduce MTTR, codify diagnostic playbooks, eliminate toil>

## Constraints
<production system access, regulated infrastructure, secrets and credentials>

## Tech Stack
<observability, IaC, cloud platforms, ticketing>
"#;

const DESIGNER_PROFILE: &str = r#"# User Profile

**Role:** Designer / creative
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** <fill in>

## What I Do
Research, ideation, prototyping, design reviews, asset production,
hand-off to engineering.

## What I'm Trying to Grow or Achieve
<fill in — e.g., faster research synthesis, design system maturity,
reduce hand-off friction>

## Constraints
<client confidentiality, brand-asset confidentiality>

## Tech Stack
<Figma, prototyping tools, asset libraries, research tooling>
"#;

const SALES_BD_PROFILE: &str = r#"# User Profile

**Role:** Sales / BD
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** <fill in>

## What I Do
Outbound prospecting, qualification, discovery calls, demos, follow-ups,
pipeline hygiene, reporting.

## What I'm Trying to Grow or Achieve
<fill in — e.g., increase reply rate, qualify faster, deal velocity>

## Constraints
<CRM data privacy, prospect confidentiality, deal-stage information>

## Tech Stack
<CRM, sales engagement, prospecting tools, comms>
"#;

const CUSTOM_PROFILE: &str = r#"# User Profile

**Role:** <fill in>
**Company:** <fill in>
**Industry:** <fill in>
**Team Size:** <fill in>
**Stage:** <fill in>

## What I Do
<free-text description of day-to-day work>

## What I'm Trying to Grow or Achieve
<strategic goals — feeds Tier 3 synthesis>

## Constraints
<confidentiality, regulatory, tooling constraints Claude should know about>

## Tech Stack
<primary tools and platforms>
"#;

const DEFAULT_TIERS: &str = r#"{
  "schema_version": 1,
  "tiers": [
    {
      "id": "time-reclaimers",
      "name": "Time Reclaimers",
      "description": "Tactical automation of repetitive or multi-tool workflows",
      "weight": 1.0,
      "scoring": "quantitative",
      "example_shapes": [
        "Repeated sequence of clicks/commands 5+ times per week",
        "Multi-tool orchestration chains",
        "Templated communication"
      ],
      "enabled": true
    },
    {
      "id": "expertise-amplifiers",
      "name": "Expertise Amplifiers",
      "description": "Knowledge capture, team leverage, diagnostic workflows",
      "weight": 1.2,
      "scoring": "quantitative",
      "example_shapes": [
        "Diagnostic patterns applied to recurring issues",
        "Expertise that lives in one person's head",
        "Research and synthesis across sources"
      ],
      "enabled": true
    },
    {
      "id": "capability-unlocks",
      "name": "Capability Unlocks",
      "description": "Strategic opportunities — new offerings or market gaps",
      "weight": 1.5,
      "scoring": "qualitative",
      "example_shapes": [
        "Things currently too manual to be profitable",
        "Productizable workflows from client delivery",
        "Team leverage multipliers"
      ],
      "enabled": true
    }
  ]
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_have_unique_ids() {
        let mut ids: Vec<&str> = STARTER_TEMPLATES.iter().map(|t| t.id).collect();
        ids.sort();
        let original_len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), original_len);
    }

    #[test]
    fn all_eight_archetypes_present() {
        assert_eq!(STARTER_TEMPLATES.len(), 8);
    }

    #[test]
    fn find_template_returns_known_ids() {
        assert!(find_template("solo-engineer").is_some());
        assert!(find_template("custom").is_some());
        assert!(find_template("does-not-exist").is_none());
    }

    #[test]
    fn default_tiers_is_valid_json() {
        let _: serde_json::Value = serde_json::from_str(DEFAULT_TIERS).unwrap();
    }
}
