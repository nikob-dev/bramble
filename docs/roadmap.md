# Bramble roadmap

Self-learning pet-banks that teach electronic payments. This is the approved architecture and milestone plan (2026-10-11). Decisions made while building live in [decisions.md](decisions.md).

## Goal
A Rust pet that lives in the terminal (TUI) and the browser (Leptos/WASM). It teaches **developers and fintech-curious learners** how electronic payments work: ledgers, card and bank rails, crypto/stablecoins, and security/compliance.

The pet is a **self-learning teacher**. It learns from curated industry feeds through Claude, submits what it learned to **peer review by other pets (a DAO)**, and calibrates what and how it teaches from the learner's measured progress.

**Each pet is a bank.** Several pet containers on one machine form a payment network that settles over simulated rails.

## Core loop
1. The pet **asks for help**: "a customer says they were charged twice, what happened?"
2. You investigate in the **terminal lab** with real tools (`bramble ledger`, `sqlite3`, `curl`, `openssl`).
3. Solving it **teaches the pet**. Its stats grow and its bank gets healthier.
4. Neglect has consequences: unreconciled entries pile up, the pet gets "hungry", and eventually it suffers a mini bank run.
5. **News moments:** a pet learned something from the world, and you're asked to act as a **validator** before the network accepts it.

## Architecture
```
docker compose: pet-a  pet-b  pet-c   (+ dev toolchain; profile chain → anvil)
                  └──── libp2p gossipsub + mDNS (BRAMBLE_PEERS fallback) ────┘
                  versioned signed envelopes: {v, kind, from, ts, nonce, payload, sig}

one container = `bramble node` (runs as user `pet`)
  Bank    — double-entry ledger (SQLite) · Rail trait: RTP (gross/instant), ACH (net/batch)
  Teacher — concept DAG · learning objectives · BKT mastery · rule-based format choice
  Mind    — feeds → Claude extract → verify → propose → peer review → accept
  Pet     — mood/stats driven by bank health + learning; personality + specialty
  axum    — REST + WebSocket hub ──► TUI (ratatui + PTY lab, user `learner`) and Leptos CSR
```

| Crate | Role |
|---|---|
| `bramble-core` | Pure logic: pet state, concept DAG, mastery, protocol types (versioned) |
| `bramble-ledger` | Double-entry ledger, idempotency, Rail trait, RTP, ACH |
| `bramble-net` (M4) | libp2p, ed25519 identity, envelope sign/verify, replay window |
| `bramble-mind` (M5) | Feeds, Claude client, extract/verify/review, token budget |
| `bramble` | Binary: `node`, `tui`, `ledger`, lesson controls |
| `bramble-web` (M3) | Leptos CSR via trunk |

## Key designs
- **Ledger:** zero-sum entries enforced in code *and* by SQLite triggers. History is append-only, and retries are protected by idempotency keys. RTP settles each payment immediately (gross). ACH nets bilateral positions at a cutoff, so float and netting become visible.
- **Lab isolation:** the lab shell runs as `learner`, with read-only access to the ledger. Writes go through the node's API.
- **Teacher:** a concept DAG with prerequisites, BKT mastery, and rule-based format choice. An epsilon-greedy bandit replaces the rules only once there are about 50 logged sessions. The pet always explains its choices.
- **Mind + DAO (credible review, not correlated rubber-stamping):**
  1. A pet extracts claims with source quotes.
  2. **Deterministic checks** run first: allowlisted URL, quote appears verbatim, concept exists.
  3. **Diverse reviewers** (specialty, persona and model tier differ) cast signed votes with rationales.
  4. The **human validator** votes and holds a veto. Quorum is ⌈2/3⌉.
  5. A cutaway teaches "correlated validators" using this very system.
- **Safety:** feed text is untrusted. LLM output is schema-validated and never executed. Drafted lessons use declarative checks only. There is a daily token budget, and the app works offline without an API key.
- **Protocol:** envelopes carry a version, nonce and timestamp. Receivers reject bad signatures, unknown major versions and replays.

## Milestones
| # | Deliverable | Definition of done |
|---|---|---|
| M0 | Scaffold, Docker build, test harness | `make test` green; image < 150 MB |
| M1 | One pet: ledger, node API, TUI + lab, lesson *money-is-rows* | `make play` completes the lesson; ledger invariant tests pass |
| M2 | Teacher (DAG, BKT, rules) + 4 more Ledger lessons | Mastery visible in the TUI; frontier selection unit-tested |
| M3 | Leptos web: pet, ledger, mastery map, lesson panel | TUI and browser in sync over WS within 1s |
| M4 | 3 pets, `bramble-net`, RTP + ACH, rails lessons + cutaway | ACH nets at cutoff; all ledgers reconcile |
| M5 | Mind: extract → verify → propose → review → human vote → accept | Mock-Claude e2e passes; one live item accepted across pets |
| M6+ | Card rail (4-party, ISO 8583, interchange), stablecoin on anvil, Security & Compliance track | — |

## Risks
| Risk | Mitigation |
|---|---|
| Scope creep | M1 is a shippable walking skeleton; every later milestone is optional |
| mDNS is flaky on Docker bridges | Static `BRAMBLE_PEERS` fallback from day one |
| Feeds lack RSS or change format | Verify the list at M5; fall back to a curated seed list |
| LLM factual errors enter the curriculum | Verbatim-quote check, diverse reviewers, human veto, citations |
| Wrong payment facts in seeded content | Every lesson cites a primary source (Fed, Nacha, ISO, EMVCo, PCI SSC) |
