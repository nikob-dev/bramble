# Decision log

## 001 — Debian slim runtime instead of distroless (2026-10-11)
**Context:** Lessons run real commands (`bash`, `curl`, `openssl`, `sqlite3`) inside the pet's container, and learners page through files with `less`.
**Decision:** Use `debian:12.15-slim` as the runtime base and install only those tools.
**Trade-off:** Larger image and attack surface than distroless; accepted because the lab is the product.

## 002 — SQLite per pet (2026-10-11)
**Context:** Each pet is a self-contained bank in its own container.
**Decision:** One embedded SQLite file per pet in a named volume.
**Trade-off:** No shared SQL across pets (intended: pets are separate banks that must reconcile over the network).

## 003 — Add crates when their milestone starts (2026-10-11)
**Decision:** M0 creates only `bramble-core`, `bramble-ledger`, and `bramble`. `bramble-net`, `bramble-mind`, and `bramble-web` arrive with M4, M5, and M3.
**Why:** Empty crates add build time and imply structure that hasn't been designed against real code yet.

## 004 — Separate `pet` and `learner` users (2026-10-11)
**Decision:** The node runs as `pet` and owns the ledger DB; the lab shell runs as `learner` with read-only access.
**Why:** Lesson checks can't be gamed by editing the ledger directly.

## 005 — Observe the lab via private OSC markers (2026-10-11)
**Context:** Lessons need each command's text, exit code, cwd, *and output* (e.g. "did your SQL print the balance?").
**Decision:** The lab's bash emits `ESC]7771 BEL` from `PS0` (command starts) and `ESC]7770;<base64 exit\tcwd\tcmd> BEL` from `PROMPT_COMMAND` (command ends). The TUI owns the PTY, scans the byte stream for these, and captures the output between them. Terminal emulators ignore unknown OSC codes, so the same bytes render untouched.
**Trade-off:** Commands run inside other interactive programs (e.g. queries typed inside `sqlite3`) are seen as one command when that program exits. That is fine for checks on final output.

## 006 — Lesson controls are shell commands (2026-10-11)
**Decision:** `bramble next`, `bramble hint`, and `bramble answer <x>` are real CLI commands run in the lab. The CLI only acknowledges them, and the node reacts to them via the command log (005).
**Why:** Keeps the learner's hands in the terminal, needs no TUI focus switching, and works the same for any future client.

## 007 — Knowledge tracing per concept, not per objective (2026-10-11)
**Context:** The roadmap said BKT per learning objective. Each lesson has 3–5 tasks per concept, but only about one per objective, which is too little evidence to estimate anything.
**Decision:** One BKT estimate per concept (`p_init 0.2, p_learn 0.3, p_slip 0.1, p_guess 0.2`). Each finished task is one observation, scored *correct* only on a clean first attempt (no hint, stumble, or wrong answer). Objectives remain as learner-facing descriptions.
**Trade-off:** Can't tell *which* objective is weak within a concept. Revisit once concepts have several tasks per objective.

## 008 — Learner writes go through a payments API, bound to localhost (2026-10-11)
**Context:** M2 lessons need the learner to post entries, but the lab user must not write the ledger file (004).
**Decision:** `POST /api/entries` modelled on real payment APIs: `Idempotency-Key` header, 201/200 (replay)/409/422. The compose port binds to `127.0.0.1` because the API accepts writes. Raw-SQL experiments happen on a learner-owned *copy* (`~/lab/scratch.db`), where the same triggers fire.
**Why:** Teaches the API layer developers actually integrate with, keeps defense-in-depth visible, and keeps the real ledger trustworthy.

## 009 — Fast-track only skips steps that later steps don't need (2026-10-11)
**Context:** Fast-track drops `scaffold = true` steps. The first full-track e2e run showed that a dependent step (e.g. querying `scratch.db`) breaks if its setup step was skipped.
**Decision:** Any step that follows a scaffold step repeats the setup command in its prompt ("No scratch copy yet? …"). The Teacher only claims a fast-track when the next lesson actually has scaffold steps. Every task has a stable `id`, which the event log and the adaptive e2e test key on.

## 010 — The browser is a live dashboard; the terminal stays the lab (2026-10-11)
**Context:** M3 adds a browser UI. Lessons are verified by watching real shell commands, which only exist in the TUI's lab.
**Decision:** The Leptos app shows pet, lesson, mastery map, and the books, live over the same WebSocket as the TUI. It sends nothing. State messages carry a ledger view, and the node broadcasts after every API post, so a `curl` payment shows up in the browser instantly.
**Implementation notes:** WebSocket via raw `web-sys` (visible open/message/close/reconnect, no extra crates). Unknown messages are skipped, never fatal. `wasm-bindgen` is pinned exactly because trunk downloads the matching CLI. trunk 0.21.14 is installed from release binaries with **pinned SHA-256s** in the Dockerfile, so a tampered release fails the build. Text reflow and pet art moved into `bramble-core` so both clients render identically.

## 011 — Pets talk over plain HTTP with signed envelopes, not libp2p (2026-10-11)
**Context:** The roadmap planned libp2p gossipsub with mDNS discovery for M4. M4's lessons are about payment rails; the networking layer only has to deliver messages between 4 containers that Docker Compose already names.
**Decision:** Each node exposes `POST /net/envelope` on the compose network only (never published to the host). Peers come from a static `BRAMBLE_PEERS` list. Every message is an envelope `{v, kind, from, ts, nonce, payload, sig}` signed with the node's Ed25519 key. Receivers reject bad signatures, unknown major versions, stale timestamps and replayed nonces.
**Trade-off:** No discovery and no gossip. libp2p moves to M5, where many reviewers gossip proposals and that cost buys something.

## 012 — An operator node settles between banks (2026-10-11)
**Context:** Banks on real rails don't settle with each other directly. They settle through an operator: the Fed (FedACH, Fedwire) or The Clearing House (RTP).
**Decision:** A 4th container runs the same binary as `bramble node --role operator`. It holds one account per bank and moves those balances: immediately per payment for RTP, and once per cutoff, netted, for ACH. Each bank mirrors its balance at the operator in a `reserves:operator` account.
**Why:** Teaches the real model, and makes cross-bank breaks (bank vs operator) something the learner can find.

## 013 — ACH cutoff is triggered, with an optional timer (2026-10-11)
**Decision:** `bramble ach cutoff` closes the batch. `BRAMBLE_ACH_CUTOFF_SECS` adds a periodic cutoff and is off by default.
**Why:** Tests stay deterministic, and lessons decide when settlement happens. Real ACH runs several fixed settlement windows a day; the lesson text says so.

## 014 — ACH files use a Nacha subset (2026-10-11)
**Decision:** Real 94-character fixed-width records: file header (1), batch header (5), entry detail (6), batch control (8) and file control (9). The entry hash and dollar totals are computed and checked. No addenda records and no blocking-factor padding yet.
**Why:** Learners inspect authentic-looking files with `cat`, `cut` and `awk`.

## 015 — The learner banks at pet-a; the other banks are counterparties (2026-10-11)
**Decision:** `make play` attaches to pet-a (pip), as before. pet-b and pet-c have their own dashboards on host ports 8082 and 8083; the operator's read-only dashboard is on 8090. All ports bind to localhost. Only pet-a's lab is used in lessons.

## 016 — The Rails track: 4 lessons and a cutaway (2026-10-11)
**Decision:** Four lessons: instant payments (RTP), batch payments (ACH), netting & settlement, and cross-bank reconciliation. A cutaway compares RTP and ACH on speed, cost, finality, failure modes and float. Every lesson cites primary sources (Nacha, the Federal Reserve, The Clearing House).
