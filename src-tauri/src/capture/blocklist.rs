use crate::config::BlocklistConfig;

#[derive(Debug, Clone)]
pub struct Blocklist {
    apps: Vec<Pattern>,
    titles: Vec<Pattern>,
}

impl Blocklist {
    pub fn from_config(cfg: &BlocklistConfig) -> Self {
        Self {
            apps: cfg.apps.iter().map(|s| Pattern::new(s)).collect(),
            titles: cfg
                .window_title_patterns
                .iter()
                .map(|s| Pattern::new(s))
                .collect(),
        }
    }

    pub fn is_blocked(&self, app_name: Option<&str>, window_title: Option<&str>) -> Option<String> {
        if let Some(app) = app_name {
            if let Some(m) = self.apps.iter().find(|p| p.matches(app)) {
                return Some(format!("blocklist:app:{}", m.raw));
            }
        }
        if let Some(title) = window_title {
            if let Some(m) = self.titles.iter().find(|p| p.matches(title)) {
                return Some(format!("blocklist:title:{}", m.raw));
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
struct Pattern {
    raw: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone)]
enum Part {
    Literal(String),
    Wildcard,
}

impl Pattern {
    fn new(raw: &str) -> Self {
        let mut parts = Vec::new();
        let mut buf = String::new();
        for ch in raw.chars() {
            if ch == '*' {
                if !buf.is_empty() {
                    parts.push(Part::Literal(std::mem::take(&mut buf).to_lowercase()));
                }
                if !matches!(parts.last(), Some(Part::Wildcard)) {
                    parts.push(Part::Wildcard);
                }
            } else {
                buf.push(ch);
            }
        }
        if !buf.is_empty() {
            parts.push(Part::Literal(buf.to_lowercase()));
        }
        Self {
            raw: raw.to_string(),
            parts,
        }
    }

    fn matches(&self, input: &str) -> bool {
        let haystack = input.to_lowercase();
        matches_parts(&self.parts, &haystack)
    }
}

fn matches_parts(parts: &[Part], input: &str) -> bool {
    match parts.first() {
        None => input.is_empty(),
        Some(Part::Literal(lit)) => {
            if parts.len() == 1 {
                input == lit
            } else {
                input.starts_with(lit.as_str())
                    && matches_parts(&parts[1..], &input[lit.len()..])
            }
        }
        Some(Part::Wildcard) => {
            let rest = &parts[1..];
            if rest.is_empty() {
                return true;
            }
            for idx in 0..=input.len() {
                if input.is_char_boundary(idx) && matches_parts(rest, &input[idx..]) {
                    return true;
                }
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bl(apps: Vec<&str>, titles: Vec<&str>) -> Blocklist {
        Blocklist::from_config(&BlocklistConfig {
            apps: apps.into_iter().map(String::from).collect(),
            window_title_patterns: titles.into_iter().map(String::from).collect(),
            url_domains: vec![],
        })
    }

    #[test]
    fn exact_app_match_case_insensitive() {
        let b = bl(vec!["1Password.exe"], vec![]);
        assert!(b.is_blocked(Some("1password.exe"), None).is_some());
        assert!(b.is_blocked(Some("notepad.exe"), None).is_none());
    }

    #[test]
    fn wildcard_title_match() {
        let b = bl(vec![], vec!["*Bank*", "*Incognito*"]);
        assert!(b.is_blocked(None, Some("Chase Bank – Accounts")).is_some());
        assert!(b.is_blocked(None, Some("Google Chrome - Incognito")).is_some());
        assert!(b.is_blocked(None, Some("Stack Overflow")).is_none());
    }

    #[test]
    fn leading_and_trailing_wildcards() {
        let b = bl(vec!["*.banking.*"], vec![]);
        assert!(b.is_blocked(Some("chase.banking.com"), None).is_some());
        assert!(b.is_blocked(Some("banking"), None).is_none());
    }

    #[test]
    fn empty_inputs_not_blocked() {
        let b = bl(vec!["something"], vec!["*other*"]);
        assert!(b.is_blocked(None, None).is_none());
    }
}
