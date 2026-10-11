# Decision log

## 001 — Debian slim runtime instead of distroless (2026-10-11)
**Context:** Lessons run real commands (`bash`, `curl`, `openssl`, `sqlite3`) inside the pet's container.
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
