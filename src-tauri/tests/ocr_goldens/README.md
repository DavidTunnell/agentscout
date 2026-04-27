# OCR golden-image suite

The Week 2 dogfood gate requires this directory to hold ~10 screenshot
fixtures with known text content. The test harness in
`src-tauri/tests/ocr_goldens.rs` runs each fixture through the configured
OCR engine and asserts ≥85% token recall against the expected text.

## How to add a fixture

1. Take a screenshot containing recognizable text (a code editor, a
   terminal, a settings dialog — match the kinds of content AgentScout
   will see in the wild).
2. Save it as PNG inside this directory, e.g. `code-editor-01.png`.
3. Add an entry to `manifest.json`:

```json
{
  "fixtures": [
    {
      "image": "code-editor-01.png",
      "expected_text": "fn main() { println!(\"hello\"); }"
    }
  ]
}
```

You don't have to be exhaustive in `expected_text` — the recall metric
checks how many of the expected tokens appear in the extracted text.
List the tokens you actually care about (key identifiers, structural
words). Skip noisy chrome like timestamps or icons.

## Running the suite

```bash
cd src-tauri
cargo test --test ocr_goldens -- --nocapture
```

Skipped automatically if `manifest.json` is missing or empty, so CI
stays green when fixtures haven't been added yet.

## Tuning the threshold

Edit `RECALL_THRESHOLD` in `src-tauri/tests/ocr_goldens.rs`. Default is
0.85. Lower it briefly while iterating on a new engine implementation,
then ratchet back up once recall stabilizes.

## Per-fixture skip

Add `"skip_reason": "..."` to a fixture entry to skip it without
removing the manifest entry — useful for known-flaky inputs that you
want to retain for context.
