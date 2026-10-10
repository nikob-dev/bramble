# Bramble — project notes for Claude

- Purpose: teach **electronic payments** via self-learning pet-banks. The terminal is the lab, payments are the subject.
- All builds and tests run in Docker: `make test`, `make lint`. There is no local Rust toolchain.
- `bramble-core` stays pure (no IO). IO lives in `bramble` (node/TUI) and the ledger crate's storage layer.
- Lessons are TOML data under `content/`; checks must be declarative (never executable code).
- Pin new dependency versions by running `cargo search` in the dev container; record notable choices in `docs/decisions.md`.
- Branches: `feat/mN-*` per milestone; conventional commits.
