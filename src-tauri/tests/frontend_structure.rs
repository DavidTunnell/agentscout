//! Static analysis of the frontend HTML/JS files. Catches gross
//! regressions like missing nav links, broken script imports, or IDs
//! that the JS expects but the HTML no longer has.
//!
//! Cheaper than a full Playwright harness; covers the obvious wins.

use scraper::{Html, Selector};
use std::path::PathBuf;

fn frontend_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src")
}

fn read_html(name: &str) -> String {
    std::fs::read_to_string(frontend_dir().join(name))
        .unwrap_or_else(|e| panic!("reading {name}: {e}"))
}

fn read_js(name: &str) -> String {
    std::fs::read_to_string(frontend_dir().join("assets").join(name))
        .unwrap_or_else(|e| panic!("reading assets/{name}: {e}"))
}

fn doc(html: &str) -> Html {
    Html::parse_document(html)
}

fn nav_links(html: &Html) -> Vec<String> {
    let sel = Selector::parse("header nav a").unwrap();
    html.select(&sel)
        .map(|el| el.value().attr("href").unwrap_or("").to_string())
        .collect()
}

#[test]
fn every_page_has_the_same_four_nav_links() {
    for page in [
        "index.html",
        "review.html",
        "inspector.html",
        "settings.html",
    ] {
        let html = read_html(page);
        let parsed = doc(&html);
        let links = nav_links(&parsed);
        assert!(
            links.contains(&"index.html".to_string()),
            "{page}: missing Status link"
        );
        assert!(
            links.contains(&"review.html".to_string()),
            "{page}: missing Recommendations link"
        );
        assert!(
            links.contains(&"inspector.html".to_string()),
            "{page}: missing Inspector link"
        );
        assert!(
            links.contains(&"settings.html".to_string()),
            "{page}: missing Settings link"
        );
    }
}

#[test]
fn every_page_marks_exactly_one_nav_link_active() {
    for page in [
        "index.html",
        "review.html",
        "inspector.html",
        "settings.html",
    ] {
        let html = read_html(page);
        let parsed = doc(&html);
        let active_sel = Selector::parse("header nav a.active").unwrap();
        let active = parsed.select(&active_sel).count();
        assert_eq!(
            active, 1,
            "{page}: expected exactly one .active nav link, got {active}"
        );
    }
}

#[test]
fn review_page_has_required_elements_for_review_js() {
    let html = read_html("review.html");
    let d = doc(&html);
    for id in ["cycle-body", "rec-list", "show-suppressed", "refresh-btn"] {
        let sel = Selector::parse(&format!("#{id}")).unwrap();
        assert!(
            d.select(&sel).next().is_some(),
            "review.html missing #{id} that review.js depends on"
        );
    }
}

#[test]
fn inspector_page_has_required_elements_for_inspector_js() {
    let html = read_html("inspector.html");
    let d = doc(&html);
    for id in [
        "capture-grid",
        "limit-select",
        "refresh-btn",
        "detail-panel",
    ] {
        let sel = Selector::parse(&format!("#{id}")).unwrap();
        assert!(
            d.select(&sel).next().is_some(),
            "inspector.html missing #{id} that inspector.js depends on"
        );
    }
}

#[test]
fn status_page_has_required_elements_for_app_js() {
    let html = read_html("index.html");
    let d = doc(&html);
    for id in ["status-body", "toggle-pause", "tick-now"] {
        let sel = Selector::parse(&format!("#{id}")).unwrap();
        assert!(d.select(&sel).next().is_some(), "index.html missing #{id}");
    }
}

#[test]
fn each_page_links_a_module_script() {
    for (page, expected_script) in [
        ("index.html", "assets/app.js"),
        ("review.html", "assets/review.js"),
        ("inspector.html", "assets/inspector.js"),
        ("settings.html", "assets/settings.js"),
    ] {
        let html = read_html(page);
        let d = doc(&html);
        let sel = Selector::parse(&format!(
            "script[type=\"module\"][src=\"{expected_script}\"]"
        ))
        .unwrap();
        assert!(
            d.select(&sel).next().is_some(),
            "{page} should load <script type=\"module\" src=\"{expected_script}\">"
        );
    }
}

#[test]
fn js_files_invoke_only_known_tauri_commands() {
    // If somebody adds a new invoke("cmd_x") in JS but not the Rust
    // handler, this test catches it. Whitelist of registered commands
    // tracked alongside lib.rs::generate_handler!.
    const REGISTERED: &[&str] = &[
        "cmd_get_status",
        "cmd_toggle_pause",
        "cmd_run_tick_now",
        "cmd_list_recent_captures",
        "cmd_get_capture_image",
        "cmd_list_starter_templates",
        "cmd_list_recommendations",
        "cmd_set_disposition",
        "cmd_get_cycle_status",
        "cmd_get_cost_projection",
        "cmd_get_capability_info",
        "cmd_get_settings",
        "cmd_update_settings",
        // v0.5.5
        "cmd_set_anthropic_key",
        "cmd_test_anthropic_key",
        "cmd_clear_anthropic_key",
        "cmd_get_credentials_status",
        "cmd_run_cycle_now",
        // v0.5.6
        "cmd_start_setup_conversation",
        "cmd_continue_setup_conversation",
        "cmd_finalize_setup_conversation",
        "cmd_start_tier_calibration",
        "cmd_continue_tier_calibration",
        "cmd_finalize_tier_calibration",
        "cmd_get_personalization_status",
    ];
    for js in ["app.js", "review.js", "inspector.js", "settings.js"] {
        let body = read_js(js);
        for found in extract_invoke_targets(&body) {
            assert!(
                REGISTERED.contains(&found.as_str()),
                "{js} invokes unknown Tauri command: {found}"
            );
        }
    }
}

/// Hand-rolled scan for `invoke("cmd_name"` — avoids a regex dep just
/// for one test. Tolerates whitespace and any quote style in case the
/// frontend swaps string-quote conventions later.
fn extract_invoke_targets(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = js.as_bytes();
    let needle = b"invoke(";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Skip whitespace
            let mut j = i + needle.len();
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\t') {
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let quote = bytes[j];
            if quote == b'"' || quote == b'\'' || quote == b'`' {
                j += 1;
                let start = j;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                if j < bytes.len() {
                    if let Ok(name) = std::str::from_utf8(&bytes[start..j]) {
                        if name.starts_with("cmd_")
                            && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                        {
                            out.push(name.to_string());
                        }
                    }
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn no_external_resources_referenced_in_html() {
    // Privacy posture (SPEC.md §10.6): the email template is inline-CSS
    // only. The app windows can use local stylesheets but should never
    // pull from CDNs at runtime.
    for page in [
        "index.html",
        "review.html",
        "inspector.html",
        "settings.html",
    ] {
        let html = read_html(page);
        let d = doc(&html);
        let link_sel = Selector::parse("link[rel=\"stylesheet\"]").unwrap();
        for el in d.select(&link_sel) {
            let href = el.value().attr("href").unwrap_or("");
            assert!(
                !href.starts_with("http://") && !href.starts_with("https://"),
                "{page} links external stylesheet: {href}"
            );
        }
        let script_sel = Selector::parse("script[src]").unwrap();
        for el in d.select(&script_sel) {
            let src = el.value().attr("src").unwrap_or("");
            assert!(
                !src.starts_with("http://") && !src.starts_with("https://"),
                "{page} loads external script: {src}"
            );
        }
    }
}
