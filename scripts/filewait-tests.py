#!/usr/bin/env python3
"""TC-650 (docs/TEST.md): a file to send that never opens makes only that
one send fail, and the daemon keeps sending and receiving (FLOW-FILE S3,
API-LOCAL-008, ADR-069), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/filewait-tests.py <RUN-ID> [650]

alice's daemon runs with IMBO_FAULT_INJECT=file-wait, the fault point of
docs/TEST.md §3: looking at a file to send never finishes, as it doesn't
while macOS waits for someone to allow access to that folder. The case
starts its own server and seed the way parity-tests.py does, writes its
lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if set, on the
first line) and exits 1 when any step failed. Standard library only.
"""

import importlib.util
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, OUT, check = P.L, P.OUT, P.check
# A pre-fix daemon never answers an upload; the fix answers in 10 s.
GIVE_UP = 60


def connects(who):
    log = f"{L}/{who}/daemon.log"
    return open(log).read().count("connected") if os.path.exists(log) else 0


def stop_daemon(who):
    pid = f"{L}/{who}/daemon.pid"
    if os.path.exists(pid):
        subprocess.run(["kill", open(pid).read().strip()], capture_output=True)
    for _ in range(50):
        if not os.path.exists(f"{L}/{who}/daemon.sock"):
            return
        time.sleep(0.2)


def start_daemon(who, **env):
    before = connects(who)
    subprocess.run(["sh", P.ENV, "daemon", who], env={**os.environ, **env}, check=True)
    for _ in range(75):
        if connects(who) > before:
            return True
        time.sleep(0.2)
    return False


def timed(who, *args):
    """`imbo` with a cap, so a daemon that never answers doesn't hang the
    run: returns exit code, output and seconds, code 124 on the cap."""
    at = time.monotonic()
    try:
        r = subprocess.run([P.IMBO, *args], capture_output=True, text=True, timeout=GIVE_UP,
                           env={**os.environ, "IMBO_HOME": f"{L}/{who}"})
        code, out = r.returncode, P.ANSI.sub("", r.stdout + r.stderr).strip()
    except subprocess.TimeoutExpired:
        code, out = 124, f"no answer in {GIVE_UP} s"
    return code, out, round(time.monotonic() - at, 1)


def tc650():
    P.up("alice:bob")
    B = P.ids()["bob"]
    n1 = f"{L}/work/n1.txt"
    with open(n1, "w") as f:
        f.write("T650 attachment\n")
    stop_daemon("alice")
    up = start_daemon("alice", IMBO_FAULT_INJECT="file-wait")
    OUT.append(f"前置:alice 常驻程序带 IMBO_FAULT_INJECT=file-wait(起来了={up});待发文件 {n1}")

    code, out, took = timed("alice", "send", str(B), "T650-1", "--file", n1)
    waiting = "Cannot read the file" in out and "macOS" in out and n1 in out
    check(650, 1, "打不开的文件只让这次发送失败",
          code == 1 and waiting and 10 <= took <= 20, f"退出码 {code},{took} 秒,输出 {out!r}")

    code, out, took = timed("alice", "send", str(B), "T650-2")
    sent = code == 0 and "Sent" in out
    code2, inbox, _ = timed("bob", "inbox", "--all")
    check(650, 2, "同一台电脑照常发得出消息",
          sent and "T650-2" in inbox, f"发送退出码 {code},{took} 秒,输出 {out!r};bob 收件箱有 T650-2={'T650-2' in inbox}")

    code, out, took = timed("alice", "status")
    check(650, 3, "常驻程序照常答得出状态",
          code == 0 and "daemon running" in out and took < 5, f"退出码 {code},{took} 秒,输出首行 {out.splitlines()[:1]}")

    stop_daemon("alice")
    up = start_daemon("alice")
    code, out, took = timed("alice", "send", str(B), "T650-3", "--file", n1)
    code2, inbox, _ = timed("bob", "inbox", "--all")
    check(650, 4, "不再卡住后同一个文件发得出去",
          up and code == 0 and "1 file" in out and "T650-3" in inbox,
          f"重启后起来了={up},退出码 {code},{took} 秒,输出 {out!r};bob 收件箱有 T650-3={'T650-3' in inbox}")

    outbox = f"{L}/alice/outbox"
    left = os.listdir(outbox) if os.path.exists(outbox) else []
    log = open(f"{L}/alice/daemon.log").read().count("didn't open in 10 s")
    check(650, 5, "副本清掉了,日志记下了等不到",
          not left and log >= 1, f"outbox {left};日志里 {log} 行")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["650"]
    head = os.environ.get("RUN_HEADER")
    root = os.path.dirname(os.path.dirname(HERE))
    for tc in want:
        OUT.clear()
        if head:
            OUT.append(head.replace("<TC>", f"TC-{tc}"))
        globals()[f"tc{tc}"]()
        with open(f"{root}/docs/test-runs/{run.lower()}-tc-{tc}.txt", "w") as f:
            f.write("\n".join(OUT) + "\n")
    subprocess.run(["sh", P.ENV, "down"], capture_output=True)
    print(f"failed steps: {P.FAILS}")
    sys.exit(1 if P.FAILS else 0)


if __name__ == "__main__":
    main()
