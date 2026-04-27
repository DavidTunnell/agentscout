# AgentScout

> Passively observes how you spend time on your computer and emails you a ranked list of AI agent opportunities tailored to your role and goals.

**Status:** v0.1 — Week 1 scaffolding in place. Capture subsystem is functional on Windows and macOS; Linux is best-effort. OCR, analysis, email delivery, and conversational setup land in weeks 2–5.

AgentScout lives in your system tray, captures screenshots at a fixed cadence during active computer use, clusters them by application and context, and periodically synthesizes a prioritized set of recommendations via the Anthropic API. You mark each recommendation as Implemented, Not Interested, or Maybe Later, and that feedback shapes the next cycle.

The product spec lives in [`SPEC.md`](./SPEC.md) (committed when v0.1 stabilizes) and the build plan is at [`.claude/plans/agentscout-technical-parsed-hopcroft.md`](./.claude/plans/agentscout-technical-parsed-hopcroft.md).

## What gets sent where

AgentScout is built privacy-first. Here is exactly what leaves your machine:

| Data | Destination | When |
|---|---|---|
| Cluster summaries (text + representative images from each work session) | Anthropic API | Once per analysis cycle (default every ~24 active hours) |
| Synthesis prompt (your profile + tier definitions + prior disposition history + cluster summaries) | Anthropic API | Once per analysis cycle |
| Email HTML + recipient address | Gmail API (your own OAuth) | Once per analysis cycle |

And what **never** leaves your machine:

- Raw screenshots and OCR text (encrypted at rest via AES-256-GCM, key in your OS keychain)
- Cluster metadata (foreground app, window title, timestamps)
- Recommendation history and dispositions
- Your Anthropic API key and Gmail OAuth credentials (stored only in the OS keychain)
- Any telemetry, analytics, or error reports — the AgentScout project maintainers receive nothing

You can verify network activity with `tcpdump` or Wireshark. The only hosts AgentScout contacts are `api.anthropic.com` and `gmail.googleapis.com` (both TLS).

## Privacy posture

- **BYO API keys.** You supply your own Anthropic key and Google OAuth credentials. No shared keys ship with the binary.
- **Local-first storage.** All data lives in your platform data directory:
  - Windows: `%APPDATA%\AgentScout\`
  - macOS: `~/Library/Application Support/AgentScout/`
  - Linux: `~/.local/share/agentscout/`
- **Encryption at rest.** Screenshots and thumbnails are encrypted with AES-256-GCM. The key is generated at first launch and stored in the OS keychain (Windows Credential Manager, macOS Keychain, Linux Secret Service).
- **Blocklist + pause controls.** Pre-populated blocklist covers password managers, banking domains, and incognito windows. Global pause hotkey (default `Ctrl+Alt+P`). Per-monitor opt-in.
- **Transparent skips.** Every skipped tick (idle, blocklist, paused, outside work hours) is logged in the local SQLite database so you can audit what AgentScout does and doesn't do.

## Installation

Binaries for v1 will ship as `.msi` (Windows), `.dmg` (macOS), `.AppImage` and `.deb` (Linux) — see the release page once v1 ships.

To build from source:

```bash
# Rust toolchain (1.77+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node + Tauri CLI
npm install

# Generate platform icons from a source PNG (one-time, before first build)
npm run tauri icon path/to/source-1024.png

# Dev build
npm run tauri dev

# Release build
npm run tauri build
```

> **Note:** `src-tauri/icons/` is intentionally not committed in v0.1 — drop in
> a 1024×1024 source PNG and run the `tauri icon` command above to generate
> all platform-specific sizes. Final branding assets land in week 6.

## Testing

```bash
cd src-tauri

# Unit + integration tests (skips OCR goldens — see below)
cargo test --all-targets -- --skip ocr_goldens

# Smoke test: drives the full capture pipeline end-to-end against
# synthetic screenshots and mock OCR. Exits 0 on success.
cargo run --bin smoke

# Smoke test using a real display + Tesseract (if installed)
cargo run --bin smoke -- --live

# OCR golden suite — needs fixture images checked into
# tests/ocr_goldens/. See tests/ocr_goldens/README.md.
cargo test --test ocr_goldens -- --nocapture

# Lints + format check
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs all of the above on Windows, macOS, and Linux on every push to
`main`. See `.github/workflows/ci.yml`.

### Test inventory

| Layer | Coverage |
|---|---|
| **Unit** (47+ tests) | config schema, AES-GCM crypto, blocklist matching, work-hours gate, schema migrations, OCR mock, thumbnail dimensions, conversation state machines, mock Anthropic client, starter templates |
| **Integration** | `pipeline_integration.rs` — full budget pipeline against fakes; `storage_integration.rs` — DB CRUD; `ocr_goldens.rs` — token-recall harness |
| **Smoke** | `cargo run --bin smoke` — end-to-end capture → encrypt → OCR → thumbnail → DB → decrypt round-trip |

## Configuration

Your API keys go in the OS keychain — the app prompts for them on first run. Everything else lives in `config.json` in your platform data directory. The file is human-readable; edit it directly and restart AgentScout to apply changes. Schema is documented in [`SPEC.md`](./SPEC.md#63-configjson-schema).

## Cost

At default settings (5-min cadence, 3 monitors, full-image mode), expect **$15–$30/month** in Anthropic API spend for a typical knowledge worker.

Enable **budget mode** (OCR + 400px thumbnail instead of full images) to drop costs to **$6–$12/month** with a modest accuracy tradeoff.

A hard per-cycle cost ceiling (configurable, default $5) halts analysis if projected spend exceeds the limit.

## Architecture

```
src-tauri/              # Rust backend (Tauri 2.x)
├── src/
│   ├── main.rs         # Entry point
│   ├── lib.rs          # Tauri setup, tray, commands
│   ├── config/         # Typed config + migration chain
│   ├── storage/        # SQLite + AES-GCM file crypto
│   ├── capture/        # Scheduler, screenshots, activity, blocklist
│   ├── ocr/            # Local OCR + thumbnail (week 2)
│   ├── analysis/       # Clustering + synthesis (week 3)
│   ├── anthropic/      # API client trait + fixtures (week 3)
│   ├── conversation/   # Setup + tier calibration (week 3)
│   └── email/          # Gmail OAuth + disposition server (week 4)
src/                    # Frontend (static HTML/CSS/JS)
tests/                  # Fixtures + OCR goldens
```

## License

[MIT](./LICENSE) © 2026 AgentScout Contributors.

## Contributing

Post-v1. The project will accept community starter templates, tier packs, language localizations, and alternative delivery connectors (Slack, Discord, file-only). See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the workflow.
