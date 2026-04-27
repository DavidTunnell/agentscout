# Contributing to AgentScout

This project is in pre-v1 development. The contribution workflow below describes what we'll accept once v1 ships; external contributions are not actively solicited before then.

## What we'll accept post-v1

- **Starter templates** for user archetypes not covered by the default set (see [SPEC.md §2.1](./SPEC.md#21-user-archetypes))
- **Tier packs** — pre-tuned `tier-definitions.json` files for specific industries, roles, or use cases
- **Language localizations** for setup and tier calibration conversations
- **Delivery connectors** beyond Gmail (Slack, Discord, file-only, webhook)
- **Bug fixes** with accompanying test coverage

## Development workflow

1. Open an issue describing the problem or feature before writing code
2. Fork and branch from `main`
3. Run `cargo test` and `cargo clippy` — both must pass
4. Keep PRs focused on a single concern
5. Include tests for behavioral changes

## Building

```bash
npm install
npm run tauri dev
```

Requires Rust 1.77+ and Node 18+.

## License

By contributing, you agree your contributions will be licensed under the [MIT License](./LICENSE).
