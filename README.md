# Bramble

A pet that lives in your terminal and your browser, runs a tiny bank, and teaches you how electronic payments actually work: ledgers, card and bank rails, stablecoins, security and compliance.

Pets are self-learning teachers. They read payment-industry sources, propose what they learned to the other pets, and the network (a small DAO, including you as a validator) decides what enters the shared curriculum. Teaching adapts to how you're progressing.

> Status: **M0 scaffold**. See [the roadmap](#roadmap).

## Requirements
- Docker (Desktop, OrbStack, or Colima). No local Rust toolchain needed.

## Quick start
```sh
cp .env.example .env     # optional: add ANTHROPIC_API_KEY for the learning features (M5+)
make test                # run the test suite in the dev container
make build               # build the runtime image
docker compose run --rm pet-a    # prints the pet's identity
```

## Layout
| Path | Purpose |
|---|---|
| `crates/bramble-core` | Pure domain logic: pet state, curriculum graph, mastery, protocol types |
| `crates/bramble-ledger` | Double-entry ledger and payment rails |
| `crates/bramble` | The `bramble` binary (node daemon, TUI, CLI tools) |
| `content/` | Lessons and concept graph as data (TOML) |
| `docker/` | Multi-stage Dockerfile (`dev` and `runtime` targets) |
| `docs/decisions.md` | Architecture decision log |

## Roadmap
| Milestone | What you get |
|---|---|
| M0 | Scaffold, Docker build, test harness |
| M1 | One pet: ledger, node API, TUI with a terminal lab, first lesson |
| M2 | Adaptive teacher + full Ledgers track |
| M3 | Browser UI (Leptos/WASM) |
| M4 | Three pet-banks paying each other over RTP and ACH |
| M5 | Pets learn from the world; DAO peer review |
| M6+ | Card networks, stablecoins, security and compliance |
