# Fuzz harness

Targets live in `fuzz_targets/`. Run a target locally:

```bash
cargo install cargo-fuzz   # one-time
cd src-tauri/fuzz
cargo +nightly fuzz run link_query_parser -- -max_total_time=60
```

`cargo-fuzz` requires the nightly toolchain. CI does **not** run fuzzing
on every push (it's continuous, not bounded); a separate workflow can
schedule it nightly with a time budget.

## Targets

- **link_query_parser** — feeds arbitrary bytes into
  `parse_token_from_query`. Goal: never panic. Any malformed input
  must return `Err`, not `unwrap` on a missing field, not overflow on
  the integer fields, etc.

## Adding new targets

1. Create `fuzz_targets/your_target.rs` with `fuzz_target!(|data: &[u8]| {})`
2. Add a `[[bin]]` block in `Cargo.toml`
3. Run locally, then commit. Crashes write to `fuzz/artifacts/<target>/`.
