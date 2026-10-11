"""E2E smoke test: play the whole Ledgers track through the real TUI.

Drives `bramble tui` in a pseudo-terminal, like a learner at a keyboard. Each round it
reads the node's state and types the solution for the current (lesson, step id), so it
follows whatever path the Teacher chooses (fast-track, remediation, reviews).

Usage: python3 tests/e2e/tui_smoke.py <compose-project> <node-url>
Run via `make e2e`, which starts an isolated compose project and tears it down after.
Stdlib only.
"""
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time
import urllib.request

PROJECT, URL = sys.argv[1], sys.argv[2]
MAX_ROUNDS = 150
GAS_ENTRY = "{GAS_ENTRY_ID}"

LEDGER = "$BRAMBLE_DATA_DIR/ledger.db"
POST = "curl -s -X POST localhost:8080/api/entries -H 'Content-Type: application/json'"


def entry(memo, *postings, key=None):
    legs = ", ".join(f'{{"account": "{a}", "amount_minor": {n}}}' for a, n in postings)
    header = f" -H 'Idempotency-Key: {key}'" if key else ""
    return f"""{POST}{header} -d '{{"memo": "{memo}", "postings": [{legs}]}}'"""


def sql(query, db=LEDGER, readonly=True):
    flag = "-readonly " if readonly else ""
    return f'sqlite3 {flag}{db} "{query}"'


BOOKMARK = [("customer:you", 800), ("settlement:card-network", -800)]
SOLUTIONS = {
    ("money-is-rows", "list-accounts"): ["bramble ledger accounts"],
    ("money-is-rows", "list-postings"): ["bramble ledger postings customer:you"],
    ("money-is-rows", "sum-with-sql"): [sql("SELECT SUM(amount_minor) FROM postings WHERE account_id = 'customer:you';")],
    ("money-is-rows", "why-negative"): ["bramble answer b"],
    ("money-is-rows", "minor-units"): ["bramble answer b"],
    ("money-is-rows", "reserves-balance"): [sql("SELECT SUM(amount_minor) FROM postings WHERE account_id = 'reserves';")],
    ("double-entry", "pay-alex"): [entry("Pay Alex", ("customer:you", 1200), ("customer:alex", -1200))],
    ("double-entry", "try-to-cheat"): [entry("Free money", ("customer:you", -100000000))],
    ("double-entry", "copy-ledger"): [f"cp {LEDGER} ~/lab/scratch.db"],
    ("double-entry", "forge-with-sql"): [
        f"cp {LEDGER} ~/lab/scratch.db",
        sql("INSERT INTO journal_entries (id, memo) VALUES (999, 'Free money'); "
            "INSERT INTO postings (entry_id, account_id, amount_minor) VALUES (999, 'customer:you', -100000000); "
            "UPDATE journal_entries SET status = 'posted' WHERE id = 999;", db="~/lab/scratch.db", readonly=False),
    ],
    ("double-entry", "books-balance"): [sql("SELECT SUM(amount_minor) FROM postings;")],
    ("double-entry", "nonzero-meaning"): ["bramble answer b"],
    ("double-entry", "valid-entry"): ["bramble answer b"],
    ("double-entry", "trial-balance"): [sql("SELECT SUM(amount_minor) FROM postings;")],
    ("idempotency", "find-duplicate"): [sql(
        "SELECT e.memo, p.amount_minor, COUNT(*) FROM postings AS p JOIN journal_entries AS e ON e.id = p.entry_id "
        "WHERE p.account_id = 'customer:you' GROUP BY e.memo, p.amount_minor HAVING COUNT(*) > 1;")],
    ("idempotency", "retry-with-key"): [entry("Bookmark", *BOOKMARK, key="bookmark-001")] * 2,
    ("idempotency", "key-conflict"): [entry("Bookmark", ("customer:you", 900), ("settlement:card-network", -900), key="bookmark-001")],
    ("idempotency", "refund-duplicate"): [entry("Reversal: duplicate Bookstore NR-0907",
                                                ("settlement:card-network", 2399), ("customer:you", -2399), key="refund-dup")],
    ("idempotency", "who-mints-keys"): ["bramble answer b"],
    ("idempotency", "retry-same-key"): ["bramble answer a"],
    ("idempotency", "fix-mistakes"): ["bramble answer b"],
    ("reconciliation", "download-statement"): ["curl -s localhost:8080/api/statements/card-network -o statement.csv"],
    ("reconciliation", "import-statement"): ['sqlite3 ~/lab/recon.db ".import --csv statement.csv statement"'],
    ("reconciliation", "find-the-break"): [
        'test -f ~/lab/recon.db || sqlite3 ~/lab/recon.db ".import --csv statement.csv statement"',
        sql(f"ATTACH '{LEDGER}' AS l; SELECT p.entry_id FROM statement AS s "
            "JOIN l.journal_entries AS e ON e.memo LIKE '%' || s.network_ref || '%' "
            "JOIN l.postings AS p ON p.entry_id = e.id AND p.account_id = 'customer:you' "
            "WHERE p.amount_minor <> CAST(s.amount_minor AS INTEGER);", db="~/lab/recon.db", readonly=False),
        f"bramble answer {GAS_ENTRY}",
    ],
    ("reconciliation", "transposition"): ["bramble answer b"],
    ("reconciliation", "divisible-by-nine"): ["bramble answer a"],
    ("reconciliation", "fixing-breaks"): ["bramble answer b"],
    ("hashing-signatures", "download-batch"): ["curl -s localhost:8080/api/files/batch-001.csv -o batch-001.csv"],
    ("hashing-signatures", "hash-batch"): ["sha256sum batch-001.csv"],
    ("hashing-signatures", "get-signature-and-key"): [
        "curl -s localhost:8080/api/files/batch-001.csv.sig -o batch-001.csv.sig",
        "curl -s localhost:8080/api/keys/node.pem -o node.pem",
    ],
    ("hashing-signatures", "verify-signature"): [
        "curl -s localhost:8080/api/files/batch-001.csv.sig -o batch-001.csv.sig",
        "curl -s localhost:8080/api/keys/node.pem -o node.pem",
        "openssl pkeyutl -verify -pubin -inkey node.pem -rawin -in batch-001.csv -sigfile batch-001.csv.sig",
    ],
    ("hashing-signatures", "tamper"): [
        "sed -i 's/2500/9500/' batch-001.csv",
        "openssl pkeyutl -verify -pubin -inkey node.pem -rawin -in batch-001.csv -sigfile batch-001.csv.sig",
    ],
    ("hashing-signatures", "hash-vs-signature"): ["bramble answer b"],
    ("hashing-signatures", "avalanche"): ["bramble answer b"],
    ("hashing-signatures", "who-can-sign"): ["bramble answer b"],
}


def state():
    with urllib.request.urlopen(f"{URL}/api/state") as response:
        return json.load(response)


def gas_entry_id():
    """What a learner reads off their SQL output; the test asks the container directly."""
    query = "SELECT id FROM journal_entries WHERE idempotency_key = 'setup:reconciliation:gas-station';"
    out = subprocess.run(
        ["docker", "compose", "-p", PROJECT, "exec", "-T", "pet-a",
         "sqlite3", "-readonly", "/var/lib/bramble/ledger.db", query],
        check=True, capture_output=True, text=True,
    )
    return out.stdout.strip()


class Terminal:
    def __init__(self):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execvp("docker", ["docker", "compose", "-p", PROJECT, "exec", "-it", "-u", "learner", "pet-a", "bramble", "tui"])
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
        self.drain(4)

    def drain(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            if select.select([self.fd], [], [], 0.1)[0]:
                try:
                    os.read(self.fd, 65536)
                except OSError:
                    return

    def type(self, command):
        os.write(self.fd, command.encode() + b"\r")
        self.drain(1.2)

    def close(self):
        os.write(self.fd, b"\x11")  # Ctrl-Q
        self.drain(1)
        os.waitpid(self.pid, 0)


def position(s):
    lesson = s.get("lesson")
    return None if lesson is None else (lesson["id"], lesson["step_index"], lesson["completed"])


def main():
    term = Terminal()
    try:
        for _ in range(MAX_ROUNDS):
            s = state()
            lesson = s.get("lesson")
            if lesson is None:
                break
            base = lesson["id"].split(":")[-1]
            if lesson["completed"] or lesson["kind"] == "explain":
                commands = ["bramble next"]
                label = "next"
            else:
                key = (base, lesson["step_id"])
                if key not in SOLUTIONS:
                    sys.exit(f"no solution for {key}")
                commands = [c.replace(GAS_ENTRY, gas_entry_id()) if GAS_ENTRY in c else c for c in SOLUTIONS[key]]
                label = lesson["step_id"]
            before = position(s)
            for command in commands:
                term.type(command)
            after = state()
            if position(after) == before:
                sys.exit(f"stuck at {before} after {label}: say={after.get('say')!r}")
            if lesson["completed"]:
                print(f"→ {after['activity']['kind']:<15} {after['activity']['reason']}")
            elif label != "next":
                print(f"  ✓ {lesson['id']:<30} {label}")
        else:
            sys.exit(f"did not finish within {MAX_ROUNDS} rounds")
    finally:
        term.close()

    final = state()
    mastery = {c["id"]: (c["status"], round(c["p_known"], 2)) for c in final["mastery"]}
    print(json.dumps({"activity": final["activity"]["kind"], "mastery": mastery}))
    unmastered = [k for k, (status, _) in mastery.items() if status != "mastered"]
    if final["activity"]["kind"] != "track_complete" or unmastered:
        sys.exit(f"track not complete; unmastered: {unmastered}")


if __name__ == "__main__":
    main()
