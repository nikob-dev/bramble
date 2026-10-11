"""E2E smoke test: drive `bramble tui` through a real PTY and complete money-is-rows.

Usage: python3 tests/e2e/tui_smoke.py <compose-project> <node-url>
Run via `make e2e`, which starts an isolated compose project and tears it down after.
Stdlib only, so it runs anywhere Python 3 and Docker are installed.
"""
import fcntl, json, os, pty, select, struct, sys, termios, time, urllib.request

PROJECT, URL = sys.argv[1], sys.argv[2]
STEPS = [
    "bramble next",
    "bramble ledger accounts",
    "bramble next",
    "bramble ledger postings customer:you",
    "ls -la",
    "sqlite3 -readonly $BRAMBLE_DATA_DIR/ledger.db \"SELECT SUM(amount_minor) FROM postings WHERE account_id = 'customer:you';\"",
    "bramble hint",
    "bramble answer a",
    "bramble answer b",
    "bramble next",
]

def state():
    with urllib.request.urlopen(f"{URL}/api/state") as r:
        return json.load(r)

def drain(fd, seconds):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                os.read(fd, 65536)
            except OSError:
                return

pid, fd = pty.fork()
if pid == 0:
    os.execvp("docker", ["docker", "compose", "-p", PROJECT, "exec", "-it", "-u", "learner", "pet-a", "bramble", "tui"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
drain(fd, 4)
for cmd in STEPS:
    before = state()["lesson"]["step_index"]
    os.write(fd, cmd.encode() + b"\r")
    drain(fd, 2.0)
    s = state()
    print(f"{cmd[:60]:<62} step {before} -> {s['lesson']['step_index']}  xp={s['pet']['xp']}")
os.write(fd, b"\x11")  # Ctrl-Q
drain(fd, 2)
os.waitpid(pid, 0)
final = state()
print(json.dumps({"completed": final["lesson"]["completed"], "pet": final["pet"]}))
sys.exit(0 if final["lesson"]["completed"] else 1)
