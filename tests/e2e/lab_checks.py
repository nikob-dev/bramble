"""E2E lab checks: the things real learners do that the scripted happy path doesn't.

Mistakes and escalation, interactive sqlite3, full-screen programs, resizing, multi-line
paste, restarts, and the localhost-only port binding. Needs a fresh pet.

Usage: python3 tests/e2e/lab_checks.py <compose-project> <node-url>
Run via `make e2e` or `make e2e-lab`, which use an isolated compose project. Stdlib only.
"""
import fcntl
import re
import socket
import struct
import subprocess
import sys
import termios
import time
import urllib.parse
import urllib.request

from tui_smoke import LEDGER, PROJECT, SOLUTIONS, URL, Terminal, sql, state

PASTE_START, PASTE_END = "\x1b[200~", "\x1b[201~"
PAY_ALEX_MULTILINE = """curl -s -X POST localhost:8080/api/entries -H 'Content-Type: application/json' \\
  -d '{"memo": "Pay Alex", "postings": [
        {"account": "customer:you",  "amount_minor": 1200},
        {"account": "customer:alex", "amount_minor": -1200}]}'"""


def drew(term, text):
    """Whether the TUI drew `text`. It positions words with cursor moves rather than
    spaces, so compare with escape sequences and whitespace removed."""
    plain = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]|\s", b"", term.output)
    return re.sub(r"\s", "", text).encode() in plain


def check(name, ok, detail=""):
    if not ok:
        sys.exit(f"✗ {name}: {detail}")
    print(f"✓ {name}")


def step(s):
    lesson = s["lesson"]
    return lesson["id"].split(":")[-1], lesson["step_id"], lesson["step_index"]


def last_learning_event():
    query = "SELECT task_id || '|' || outcome FROM learning_events ORDER BY id DESC LIMIT 1;"
    out = subprocess.run(
        ["docker", "compose", "-p", PROJECT, "exec", "-T", "pet-a",
         "sqlite3", "-readonly", "/var/lib/bramble/pet.db", query],
        check=True, capture_output=True, text=True,
    )
    return out.stdout.strip()


def advance_to(term, lesson_id, step_id, max_rounds=40):
    """Play forward with the known solutions until the given step is current."""
    for _ in range(max_rounds):
        s = state()
        lesson = s["lesson"]
        base = lesson["id"].split(":")[-1]
        if (base, lesson["step_id"]) == (lesson_id, step_id) and not lesson["completed"]:
            return s
        if lesson["completed"] or lesson["kind"] == "explain":
            term.type("bramble next")
        else:
            for command in SOLUTIONS[(base, lesson["step_id"])]:
                term.type(command)
    sys.exit(f"never reached {lesson_id}/{step_id}")


def resize(term, cols):
    fcntl.ioctl(term.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, cols, 0, 0))
    term.drain(0.5)


def keys(term, data, wait=1.0):
    term.write(data)
    term.drain(wait)


def wait_healthy(seconds=30):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"{URL}/healthz", timeout=1):
                return
        except OSError:
            time.sleep(0.5)
    sys.exit(f"node not healthy {seconds}s after restart")


def node_logs():
    out = subprocess.run(["docker", "compose", "-p", PROJECT, "logs", "--no-color", "pet-a"],
                         capture_output=True, text=True, check=True)
    return out.stdout.splitlines()


def learner(*command):
    """Runs a command in pet-a's container the way the lab's learner would."""
    return subprocess.run(["docker", "compose", "-p", PROJECT, "exec", "-T", "-u", "learner", "pet-a", *command],
                          capture_output=True, text=True)


def lan_ip():
    """The host's outward-facing address, or None (e.g. on CI runners without one)."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
            probe.connect(("192.0.2.1", 9))  # TEST-NET-1; nothing is sent for UDP connect
            ip = probe.getsockname()[0]
    except OSError:
        return None
    return None if ip.startswith("127.") else ip


def answers(host, port):
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def main():
    term = Terminal()
    try:
        # 8: a learner answering the pet in prose opens a quote with an apostrophe; the
        # continuation prompt must say how to get out, and Ctrl-C must recover.
        s = state()
        term.output = b""
        term.type("Sure thing! it's rows in a database?")
        stuck_hint = drew(term, "Ctrl-C to cancel")
        keys(term, b"\x03", 1.0)
        term.type("bramble next")
        check("prose with an apostrophe explains the stuck prompt, Ctrl-C recovers",
              stuck_hint and step(state()) != step(s),
              f"hint shown={stuck_hint}, step {step(s)} → {step(state())}")

        term.output = b""
        term.type("is it a store of value")
        check("prose without quotes gets the pet's 'this is a shell' hint",
              drew(term, "looks like a sentence"), "hint not drawn")

        # 9: a command with a newline inside quotes (as pasted JSON or a loop body has)
        # must reach the node whole, not cut at the first line.
        keys(term, (PASTE_START + 'echo "lab-multiline-first\nlab-multiline-second"' + PASTE_END + "\r").encode(), 1.5)
        observed = [line for line in node_logs() if "lab observation" in line and "lab-multiline-first" in line]
        check("multi-line commands reach the node whole",
              any("lab-multiline-second" in line for line in observed), f"logged: {observed}")

        # 4 + 5: full-screen programs and resizing must not break command observation.
        s = advance_to(term, "money-is-rows", "list-postings")
        keys(term, b"less /etc/os-release\r", 1.5)
        keys(term, b"q", 1.0)
        keys(term, b"top\r", 2.0)
        keys(term, b"q", 1.0)
        resize(term, 100)
        resize(term, 140)
        term.type(SOLUTIONS[("money-is-rows", "list-postings")][0])
        check("full-screen programs and resize keep observation working",
              step(state()) != step(s), f"still at {step(s)}")

        # 1: two wrong attempts escalate to the worked example without advancing.
        s = advance_to(term, "money-is-rows", "sum-with-sql")
        term.type(sql("SELECT 1;"))
        term.type(sql("SELECT COUNT(*) FROM postings;"))
        after = state()
        check("two stumbles reveal the worked example, step held",
              step(after) == step(s) and after["lesson"]["worked_example"],
              f"step {step(after)}, worked_example={after['lesson']['worked_example']!r}")

        # 2: the query typed inside an interactive sqlite3 session counts.
        term.type("sqlite3 -readonly $BRAMBLE_DATA_DIR/ledger.db")
        term.type("SELECT SUM(amount_minor) FROM postings WHERE account_id = 'customer:you';")
        term.type(".quit")
        after = state()
        check("interactive sqlite3 session passes the SQL step",
              step(after) != step(s), f"still at {step(after)}: say={after['say']!r}")

        # 1b: a wrong quiz answer is logged as one and holds the step. (Mastery only moves
        # when a step passes, and `say` is only pushed over the WebSocket.)
        s = advance_to(term, "money-is-rows", "why-negative")
        term.type("bramble answer a")
        after = state()
        event = last_learning_event()
        check("wrong quiz answer is logged, step held",
              step(after) == step(s) and event == "why-negative|wrong_answer",
              f"step {step(after)}, last event {event!r}")

        # 3: pasting the multi-line worked example (bracketed paste, like a real terminal).
        s = advance_to(term, "double-entry", "pay-alex")
        keys(term, (PASTE_START + PAY_ALEX_MULTILINE + PASTE_END + "\r").encode(), 2.0)
        after = state()
        memos = [e["memo"] for e in after["ledger"]["recent_entries"]]
        check("multi-line paste posts the payment and passes the step",
              "Pay Alex" in memos and step(after) != step(s),
              f"memos={memos}, step {step(after)}, say={after['say']!r}")
    finally:
        term.close()

    # 6: progress and mastery survive a restart.
    before = state()
    subprocess.run(["docker", "compose", "-p", PROJECT, "restart", "pet-a"], check=True, capture_output=True)
    wait_healthy()
    after = state()
    check("progress and mastery survive a restart",
          (step(after), after["mastery"]) == (step(before), before["mastery"]),
          f"{step(before)} → {step(after)}")

    # 10: the Rails CLI as a learner runs it, with no operator on this network.
    pending = learner("bramble", "ach", "pending")
    check("`bramble ach pending` with nothing queued says so and succeeds",
          pending.returncode == 0 and "No ACH payments" in pending.stderr, pending.stderr)
    for command in (["bramble", "reconcile"], ["bramble", "ach", "cutoff"]):
        result = learner(*command)
        check(f"`{' '.join(command)}` without an operator fails clearly",
              result.returncode != 0 and "operator:8080" in result.stderr, result.stderr[-300:])

    # 7: the API answers on loopback only.
    port = urllib.parse.urlparse(URL).port
    check("API answers on 127.0.0.1", answers("127.0.0.1", port))
    ip = lan_ip()
    if ip is None:
        print("skip API refuses the LAN address (no LAN address on this machine)")
    else:
        check(f"API refuses the LAN address {ip}", not answers(ip, port), "port is reachable from the network")


if __name__ == "__main__":
    main()
