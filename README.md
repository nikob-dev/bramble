# Bramble

A pet that lives in your terminal and your browser, runs a tiny bank, and teaches you how electronic payments actually work: ledgers, card and bank rails, stablecoins, security and compliance.

Pets are self-learning teachers. They read payment-industry sources, propose what they learned to the other pets, and the network (a small DAO, including you as a validator) decides what enters the shared curriculum. Teaching adapts to how you're progressing.

> Status: **M3**: a live browser dashboard alongside the terminal. See [the roadmap](#roadmap).

## Requirements
- Docker (Desktop, OrbStack, or Colima). No local Rust toolchain needed.

## Quick start
```sh
make play     # start your pet's bank node (building it the first time) and open the TUI
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
| `curl localhost:8080/api/...` | The pet-bank's payments API (entries, statements, files, keys) |

### The Ledgers track
| Lesson | You'll… |
|---|---|
| Money is rows | compute a balance yourself with SQL |
| Double-entry | pay Alex through the payments API, then watch the database reject a forged entry |
| Idempotency | find a double charge, retry safely with an `Idempotency-Key`, refund with a reversing entry |
| Reconciliation | join the card network's settlement report against the ledger and find the break |
| Hashing & signatures | verify a payment batch with `sha256sum` and an Ed25519 signature, then catch tampering |

Your pet decides what comes next and tells you why. It tracks mastery per concept (the **mastery**
column, shown on terminals ≥120 columns wide), sends you back for practice when something isn't solid,
skips warm-ups when you're flying, and schedules spaced reviews (1, 3, 7, 21 days).

`Ctrl-Q` leaves the TUI; your pet keeps running (`make down` stops it). Progress persists in a Docker volume.
Open **http://127.0.0.1:8081** for the live dashboard: your pet, the lesson, the mastery map, and the books. It updates the moment you do something in the lab, including payments you `curl` from the host. Localhost only.

## Development
```sh
make test     # unit + integration tests in the dev container
make lint     # rustfmt + clippy (-D warnings)
make e2e      # plays the whole track through the real TUI in an isolated compose project
              # (PYTHON=/path/to/python3 to pick an interpreter)
make up       # rebuild and restart after code changes
make reset    # wipe your pet back to lesson 1
make clean-docker  # reclaim disk: Docker build cache + dangling images (keeps volumes)
make logs     # structured JSON logs from the node
```

## Layout
| Path | Purpose |
|---|---|
| `crates/bramble-core` | Pure domain logic: pet state, curriculum graph, mastery, protocol types |
| `crates/bramble-ledger` | Double-entry ledger and payment rails |
| `crates/bramble` | The `bramble` binary (node daemon, TUI, CLI tools) |
| `crates/bramble-web` | Browser dashboard (Leptos → WebAssembly, built with trunk) |
| `content/` | Lessons and concept graph as data (TOML) |
| `docker/` | Multi-stage Dockerfile (`dev` and `runtime` targets) |
| `docs/roadmap.md` | Architecture and milestone plan |
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
