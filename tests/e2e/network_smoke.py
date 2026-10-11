"""E2E network check, the M4 definition of done: ACH nets at cutoff and all ledgers reconcile.

Brings up three pet-banks and the operator, sends instant (RTP) and batched (ACH) payments
between them, rings the ACH cutoff, then checks every bank's books against the operator's
statement of its account, and runs `bramble reconcile` in a bank's lab as a learner would.

Usage: python3 tests/e2e/network_smoke.py <compose-project>
Run via `make e2e-net`, which starts an isolated compose project and tears it down after.
Stdlib only.
"""
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

PROJECT = sys.argv[1]
BANKS = {"pet-a": 18281, "pet-b": 18282, "pet-c": 18283}
OPERATOR = 18290
OPENING = 1_000_000

# (rail, from, to, amount_minor, idempotency key)
PAYMENTS = [
    ("rtp", "pet-a", "pet-b", 1_200, "r1"),
    ("rtp", "pet-b", "pet-c", 800, "r2"),
    ("rtp", "pet-c", "pet-a", 300, "r3"),
    ("ach", "pet-a", "pet-b", 1_000, "a1"),
    ("ach", "pet-a", "pet-c", 500, "a2"),
    ("ach", "pet-b", "pet-a", 700, "a3"),
    ("ach", "pet-c", "pet-a", 2_000, "a4"),
    ("ach", "pet-c", "pet-b", 50, "a5"),
]


def check(name, ok, detail=""):
    if not ok:
        sys.exit(f"✗ {name}: {detail}")
    print(f"✓ {name}")


def call(method, port, path, body=None, key=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if key:
        request.add_header("Idempotency-Key", key)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            text = response.read().decode()
            return response.status, json.loads(text) if text else None
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read().decode() or "null")


def compose(*args):
    return subprocess.run(["docker", "compose", "-p", PROJECT, *args], capture_output=True, text=True)


def wait_for_peers(timeout=60):
    """Every node must have discovered the other three before money can move."""
    deadline = time.time() + timeout
    nodes = [*BANKS, "operator"]
    while time.time() < deadline:
        counts = {}
        for node in nodes:
            out = compose("exec", "-T", node, "curl", "-fsS", "localhost:9090/net/peers").stdout
            counts[node] = len(json.loads(out)) if out.strip() else 0
        if all(n == 3 for n in counts.values()):
            return
        time.sleep(1)
    sys.exit(f"✗ peers never all discovered each other: {counts}")


def books(bank):
    _, state = call("GET", BANKS[bank], "/api/state")
    return {a["account"]: a["balance_minor"] for a in state["ledger"]["accounts"]}


def pay(rail, frm, to, amount, key):
    body = {"from_account": "customer:you", "to_bank": to, "to_account": "customer:alex",
            "amount_minor": amount, "memo": f"{rail} {key}"}
    return call("POST", BANKS[frm], f"/api/{rail}/payments", body, key)


def expected_reserves():
    nets = {bank: 0 for bank in BANKS}
    for _, frm, to, amount, _ in PAYMENTS:
        nets[frm] -= amount
        nets[to] += amount
    return {bank: OPENING + net for bank, net in nets.items()}


def wait_until_cleared(timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        all_books = {bank: books(bank) for bank in BANKS}
        if all(b["clearing:ach-out"] == 0 and b["clearing:ach-in"] == 0 for b in all_books.values()):
            return all_books
        time.sleep(0.5)
    sys.exit(f"✗ clearing accounts never emptied: {all_books}")


def main():
    wait_for_peers()
    print("✓ all four nodes discovered each other")

    for rail, frm, to, amount, key in PAYMENTS:
        status, body = pay(rail, frm, to, amount, key)
        check(f"{rail.upper()} {frm} → {to} {amount}", status == 201, f"{status} {body}")
    status, body = pay(*PAYMENTS[0])
    check("RTP retry with the same key is a replay", status == 200 and body.get("replayed"), f"{status} {body}")
    check("ACH is still queued before cutoff", books("pet-a")["clearing:ach-out"] == -1_500)

    status, report = call("POST", OPERATOR, "/api/ach/cutoff")
    check("cutoff settles the window", status == 200 and report["settled"] and report["payments"] == 5,
          f"{status} {report}")

    all_books = wait_until_cleared()
    print("✓ every bank's clearing accounts are empty")
    expected = expected_reserves()
    operator_total = 0
    for bank, port in BANKS.items():
        ours = all_books[bank]["reserves:operator"]
        _, statement = call("GET", OPERATOR, f"/api/statements/{bank}")
        theirs = statement["balance_minor"]
        operator_total += theirs
        check(f"{bank} reconciles with the operator ({ours})", ours == theirs == expected[bank],
              f"ours={ours} operator={theirs} expected={expected[bank]}")
        check(f"{bank}'s books balance", sum(all_books[bank].values()) == 0, all_books[bank])
    check("the operator holds exactly the banks' reserves", operator_total == OPENING * len(BANKS))

    result = compose("exec", "-T", "-u", "learner", "pet-a", "bramble", "reconcile")
    check("`bramble reconcile` in pet-a's lab agrees", result.returncode == 0 and "Reconciled" in result.stdout,
          result.stdout + result.stderr)


if __name__ == "__main__":
    main()
