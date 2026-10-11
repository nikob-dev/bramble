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
