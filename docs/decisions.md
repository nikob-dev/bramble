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
