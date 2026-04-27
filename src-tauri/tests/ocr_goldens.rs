//! OCR golden-image suite — Week 2 dogfood gate.
//!
//! Per the build plan: "10 screenshots with known text. Pass threshold = 85%
//! token recall."
//!
//! Fixtures live in `tests/ocr_goldens/` with a manifest that pairs each
//! image with its expected text. The test computes token-level recall
//! (lowercased, punctuation-stripped) and asserts each fixture meets the
//! configured threshold.
//!
//! Skipped entirely if `tests/ocr_goldens/manifest.json` is missing — keeps
//! CI green when fixtures haven't been added yet. See the README in that
//! directory for how to add them.

use agentscout::ocr::{OcrEngine, TesseractCliEngine};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;

const RECALL_THRESHOLD: f32 = 0.85;

#[derive(Debug, Deserialize)]
struct Manifest {
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    image: String,
    expected_text: String,
    #[serde(default)]
    skip_reason: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ocr_goldens")
}

fn tokenize(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn token_recall(extracted: &str, expected: &str) -> f32 {
    let extracted_tokens = tokenize(extracted);
    let expected_tokens = tokenize(expected);
    if expected_tokens.is_empty() {
        return 1.0;
    }
    let hits = expected_tokens
        .iter()
        .filter(|t| extracted_tokens.contains(*t))
        .count();
    hits as f32 / expected_tokens.len() as f32
}

#[tokio::test]
async fn ocr_goldens_meet_recall_threshold() {
    let manifest_path = fixtures_dir().join("manifest.json");
    if !manifest_path.exists() {
        eprintln!(
            "skipping ocr_goldens test — no manifest at {}. \
             See tests/ocr_goldens/README.md.",
            manifest_path.display()
        );
        return;
    }

    let manifest_bytes = std::fs::read(&manifest_path).expect("reading manifest");
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).expect("manifest must be valid JSON");

    if manifest.fixtures.is_empty() {
        eprintln!("ocr_goldens manifest is empty; nothing to test");
        return;
    }

    let tessdata = std::env::temp_dir().join("agentscout-ocr-goldens-tessdata");
    let engine = match TesseractCliEngine::new(tessdata) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "skipping ocr_goldens — tesseract CLI unavailable: {:#}",
                e
            );
            return;
        }
    };

    let mut failures: Vec<String> = Vec::new();
    for fixture in &manifest.fixtures {
        if let Some(reason) = &fixture.skip_reason {
            eprintln!("skipping {}: {}", fixture.image, reason);
            continue;
        }
        let img_path = fixtures_dir().join(&fixture.image);
        let bytes = match std::fs::read(&img_path) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("missing fixture {}: {}", fixture.image, e));
                continue;
            }
        };
        let result = match engine.extract(&bytes).await {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("extract failed for {}: {:#}", fixture.image, e));
                continue;
            }
        };
        let recall = token_recall(&result.text, &fixture.expected_text);
        eprintln!(
            "{:>6.1}% recall — {} ({} tokens extracted)",
            recall * 100.0,
            fixture.image,
            result.token_count()
        );
        if recall < RECALL_THRESHOLD {
            failures.push(format!(
                "{} below threshold: {:.1}% < {:.1}%",
                fixture.image,
                recall * 100.0,
                RECALL_THRESHOLD * 100.0
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} OCR golden(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn token_recall_handles_punctuation() {
    let extracted = "Hello, world! Foo bar.";
    let expected = "hello world foo bar";
    let recall = token_recall(extracted, expected);
    assert!(
        (recall - 1.0).abs() < f32::EPSILON,
        "expected 100% recall, got {}",
        recall
    );
}

#[test]
fn token_recall_partial() {
    let extracted = "the quick brown";
    let expected = "the quick brown fox jumps";
    let recall = token_recall(extracted, expected);
    assert!(
        (recall - 0.6).abs() < 0.01,
        "expected ~60%, got {}",
        recall
    );
}
