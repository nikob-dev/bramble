# Bramble

A pet that lives in your terminal and your browser, runs a tiny bank, and teaches you how electronic payments actually work: ledgers, card and bank rails, stablecoins, security and compliance.

Pets are self-learning teachers. They read payment-industry sources, propose what they learned to the other pets, and the network (a small DAO, including you as a validator) decides what enters the shared curriculum. Teaching adapts to how you're progressing.

> Status: **M1 walking skeleton**: one pet-bank, a terminal lab, and the first lesson. See [the roadmap](#roadmap).

## Requirements
- Docker (Desktop, OrbStack, or Colima). No local Rust toolchain needed.

## Quick start
```sh
make play     # build, start your pet's bank node, and open the TUI
```
The top half shows your pet and the current lesson. The bottom half is a real bash shell, the **lab**.
Everything you do happens there:

| In the lab | What it does |
|---|---|
| `bramble next` | Continue past an explanation |
| `bramble hint` | Ask your pet for a hint |
| `bramble answer <letter>` | Answer a quiz |
| `bramble ledger accounts` | List the bank's accounts and balances |
| `bramble ledger postings [account]` | Show the raw rows behind a balance |
| `sqlite3 -readonly $BRAMBLE_DATA_DIR/ledger.db` | Query the ledger directly |

`Ctrl-Q` leaves the TUI; your pet keeps running (`make down` stops it). Progress persists in a Docker volume.
The node also serves `http://localhost:8081/api/state`. The browser UI arrives in M3.

## Development
```sh
make test     # unit + integration tests in the dev container
make lint     # rustfmt + clippy (-D warnings)
make e2e      # drives the real TUI through a PTY in an isolated compose project
              # (PYTHON=/path/to/python3 to pick an interpreter)
make logs     # structured JSON logs from the node
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
