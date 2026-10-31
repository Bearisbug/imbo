#!/usr/bin/env python3
"""TC-371 to TC-373 (docs/TEST.md): @ mentions in group messages
(RULE-015), the other sessions named on a group message (ADR-052) and the
3-minute reminder, against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/consult-tests.py <RUN-ID> [371 372 373]

Each case starts its own server and seed the way parity-tests.py does, and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed. TC-373 waits out
the 3 minutes, so it takes about four.
"""

import importlib.util
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, M, OUT, call, check, imbo = P.L, os.environ["M"], P.OUT, P.call, P.check, P.imbo
LOCAL_OP = os.path.join(HERE, "local-op.py")
SID = "55555555-aaaa-bbbb-cccc-000000"


def fake(who, sid, title):
    return imbo(who, "dev", "fake-session", f"{SID}{sid}", "--title", title)


def mcp(who, tool, args, session):
    r = subprocess.run(["python3", M, f"{L}/{who}", tool, json.dumps(args), "--session", session],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


def op(who, *args):
    r = subprocess.run(["python3", LOCAL_OP, f"{L}/{who}", *args], capture_output=True, text=True)
    return r.stdout.strip()


def inbox_of(who):
    _, b, _ = call("GET", "/v1/inbox?all=true", bearer=P.computer_token(f"{L}/{who}"))
    return b["items"]


def got(tag, *people):
    """Short IDs of the sessions among `people` that got the message with `tag`."""
    return sorted(i["agent"]["short_id"] for who in people for i in inbox_of(who) if tag in i["message"]["body"])


def tc371():
    P.up("alice:bob,alice:carol")
    i = P.ids()
    g = P.make_group("t371", [("bob", i["bob"]), ("carol", i["carol"])])
    for sid, title in (("371001", "前端"), ("371002", "api"), ("371003", "api-v2")):
        fake("alice", sid, title)
    imbo("alice", "group", "sessions", "t371", "--sessions", "371001,371002,371003")
    fake("bob", "371004", "Docs")
    imbo("bob", "group", "sessions", "t371", "--sessions", "371004")
    a = P.app_token("alice@imbo.test")
    OUT.append(f"前置:群 t371 #{g}")
    steps = [
        ("@前端 看一下 T371-1", ["371001"], "点名字"),
        ("@API please T371-2", ["371002"], "不分大小写"),
        ("ping @api-v2: T371-3", ["371003"], "取最长的名字"),
        ("@371004 T371-4", ["371004"], "点短 ID"),
        ("@apix T371-5", ["371001", "371002", "371003", "371004"], "紧跟字母不算点名"),
        ("@前端 @Docs T371-6", ["371001", "371004"], "点两个"),
    ]
    for n, (body, want, what) in enumerate(steps, 1):
        code, out = imbo("carol", "send", f"#{g}", body)
        time.sleep(1)
        seen = got(body.split()[-1], "alice", "bob")
        check(371, n, what, code == 0 and seen == want, f"carol 发「{body}」;收到的会话 {seen}")
    _, cs, _ = call("GET", "/v1/conversations", bearer=a)
    conv = next(c for c in cs["conversations"] if (c.get("group") or {}).get("name") == "t371")
    _, ms, _ = call("GET", f"/v1/conversations/{conv['id']}/messages", bearer=a)
    bodies = [m["body"] for m in ms["messages"]]
    tags = [f"T371-{n}" for n in range(1, 7)]
    have = [t for t in tags if any(b.endswith(t) for b in bodies)]
    check(371, 7, "App 里的群对话", have == tags, f"群对话里有 {have}")


def tc372():
    P.up("alice:bob,alice:carol")
    i = P.ids()
    A = i["alice"]
    g = P.make_group("t372", [("bob", i["bob"]), ("carol", i["carol"])])
    fake("alice", "372001", "a372a")
    fake("alice", "372002", "a372b")
    imbo("alice", "group", "sessions", "t372", "--sessions", "372001,372002")
    fake("carol", "372003", "c372")
    imbo("carol", "group", "sessions", "t372", "--sessions", "372003")
    OUT.append(f"前置:群 t372 #{g}")
    imbo("bob", "send", f"#{g}", "T372 谁来看一下")
    time.sleep(1)
    out = mcp("alice", "inbox", {}, f"claude-code:{SID}372001")
    want = f'Your other sessions "a372b" ({A}/372002) also got this group message.'
    rest = "message them with Imbo's send tool to say whether you plan to reply and what"
    tail = out[out.find("T372"):] if "T372" in out else ""
    check(372, 1, "inbox 工具", "T372" in out and want in tail and rest in tail and "3 minutes" in tail,
          f"T372:{'有' if 'T372' in out else '没有'};「{want}」:{'有' if want in tail else '没有'};"
          f"商量的做法:{'有' if rest in tail else '没有'}")
    fake("alice", "372001", "a372a")
    r = json.loads(op("alice", "hook-prompt", "claude-code", f"{SID}372002") or "{}")
    ctx = ((r.get("body") or {}).get("context")) or ""
    want = f'Your other sessions "a372a" ({A}/372001) also got this group message.'
    check(372, 2, "发话时带进", "T372" in ctx and want in ctx,
          f"context 含 T372:{'是' if 'T372' in ctx else '否'};「{want}」:{'有' if want in ctx else '没有'}")
    out = mcp("carol", "inbox", {}, f"claude-code:{SID}372003")
    check(372, 3, "只有一个会话的人", "T372" in out and "Your other sessions" not in out,
          f"T372:{'有' if 'T372' in out else '没有'};Your other sessions:{'有' if 'Your other sessions' in out else '没有'}")


def tc373():
    P.up("alice:bob")
    i = P.ids()
    g = P.make_group("t373", [("bob", i["bob"])])
    z1, z2 = "sess_tc373a373001", "sess_tc373b373002"
    pid = open(f"{L}/alice/daemon.pid").read().strip()
    op("alice", "register", "zcode", z1, pid)
    op("alice", "register", "zcode", z2, pid)
    imbo("alice", "group", "sessions", "t373", "--sessions", "373001,373002")
    OUT.append(f"前置:群 t373 #{g}")

    def wait(z, timeout):
        return subprocess.Popen(["python3", LOCAL_OP, f"{L}/alice", "wait", "zcode", z, "--timeout", str(timeout)],
                                stdout=subprocess.PIPE, text=True)

    w1, w2 = wait(z1, 60), wait(z2, 60)
    time.sleep(1)
    t0 = time.time()
    imbo("bob", "send", f"#{g}", "T373 有人在吗")
    o1, o2 = w1.communicate(timeout=10)[0], w2.communicate(timeout=10)[0]
    took = time.time() - t0
    note = "Imbo: 1 new message(s) from bob"
    check(373, 1, "两个监听都收到通知", note in o1 and note in o2 and took < 5,
          f"{took:.1f} 秒;Z1:{'有' if note in o1 else '没有'}通知,Z2:{'有' if note in o2 else '没有'}通知")
    sent = mcp("alice", "send", {"to": f"#{g}", "body": "T373 from Z2"}, f"zcode:{z2}")
    time.sleep(1)
    in1 = mcp("alice", "inbox", {}, f"zcode:{z1}")
    in2 = mcp("alice", "inbox", {}, f"zcode:{z2}")
    ok = "T373 有人在吗" in in1 and "T373 from Z2" in in1 and "T373 有人在吗" in in2
    check(373, 2, "Z2 在群里发话,两边取走", ok and "error" not in sent.lower(),
          f"发出:{sent.strip().splitlines()[0] if sent.strip() else '无输出'};Z1 取到两条:{'是' if 'T373 from Z2' in in1 else '否'};"
          f"Z2 取到 T373:{'是' if 'T373 有人在吗' in in2 else '否'}")
    w1, w2 = wait(z1, 240), wait(z2, 240)
    o1 = w1.communicate(timeout=260)[0]
    at = time.time() - t0
    o2 = w2.communicate(timeout=260)[0]
    text = (json.loads(o1).get("text") or "") if o1.strip().startswith("{") else o1
    want = "Imbo: it's been 3 minutes since a message in group t373"
    check(373, 3, "没发话的收到提醒,发过话的不收",
          text.startswith(want) and 170 <= at <= 200 and "Decide now" in text and o2.strip() == "timeout",
          f"Z1 在 t0 后 {at:.0f} 秒收到:{text[:120]!r};Z2:{o2.strip()[:60]!r}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["371", "372", "373"]
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
