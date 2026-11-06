#!/usr/bin/env python3
"""TC-561 to TC-563 and TC-595 (docs/TEST.md): replying to a message or to a
passage of it (FLOW-REPLY, API-015, API-043, API-024, API-MCP-001, INV-035),
against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/reply-tests.py <RUN-ID> [561 562 563 595]

Each case starts its own server and seed the way parity-tests.py does and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed. About a minute.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, M, OUT, call, check, imbo, typ = P.L, os.environ["M"], P.OUT, P.call, P.check, P.imbo, P.typ
ENV = os.path.join(HERE, "local-env.sh")
PNG = os.path.expanduser("~/.cache/imbo-test/media/T531-small.png")


def tok(who):
    return P.computer_token(f"{L}/{who}")


def send(who, req):
    """API-015 as `who`'s terminal: (status, body)."""
    s, b, _ = call("POST", "/v1/messages", {"client_msg_id": uuid.uuid4().hex, **req}, bearer=tok(who))
    return s, b or {}


def read(who, mid):
    """API-024 as `who`: the message, or {} when refused."""
    s, b, _ = call("GET", f"/v1/messages/{mid}", bearer=tok(who))
    return (b or {}).get("message", {}) if s == 200 else {}


def sent(out):
    m = re.search(r"#(\d+)", out)
    return int(m.group(1)) if m else None


def mcp(who, tool, args, sid):
    r = subprocess.run(["python3", M, f"{L}/{who}", tool, json.dumps(args), "--session", f"claude-code:{sid}"],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


def inbox_all(who):
    return imbo(who, "inbox", "--all")[1]


def tc561():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    A, B = ids["alice"], ids["bob"]
    g = P.make_group("t561", [("bob", B)])

    rc, out = imbo("bob", "send", str(A), "T561 今天发版吗")
    m1 = sent(out)
    check(561, 1, "bob sends", rc == 0 and m1 is not None, out)

    s, b = send("alice", {"to": str(B), "body": "T561 下午三点", "quote": m1})
    m2 = b.get("message_id")
    v = read("bob", m2)
    check(561, 2, "a reply to bob's message", s == 201
          and v.get("quote") == {"id": m1, "from_name": "bob", "preview": "T561 今天发版吗"}
          and v.get("reply_to") == str(A), f"{s} #{m2} quote={v.get('quote')} reply_to={v.get('reply_to')}")

    s, b = send("alice", {"to": str(B), "body": "T561 补一句", "quote": m2})
    r = read("bob", b.get("message_id")).get("quote", {})
    check(561, 3, "a reply to my own message", s == 201 and r.get("from_name") == "alice"
          and r.get("preview") == "T561 下午三点", f"{s} {r}")

    long = "T561 a\nb" + "字" * 90
    rc, out = imbo("bob", "send", str(A), long)
    s, b = send("alice", {"to": str(B), "body": "T561 回长的", "quote": sent(out)})
    p = read("bob", b.get("message_id")).get("quote", {}).get("preview", "")
    check(561, 4, "the first 80 characters, line breaks as spaces", s == 201 and len(p) == 80
          and p.startswith("T561 a b字") and "\n" not in p, f"{s} {len(p)} chars: {p}")

    rc, out = imbo("bob", "send", str(A), "--file", PNG)
    m6 = sent(out)
    s, b = send("alice", {"to": str(B), "body": "T561 回文件", "quote": m6})
    p = read("bob", b.get("message_id")).get("quote", {}).get("preview")
    check(561, 5, "a message with only a file shows its name", rc == 0 and s == 201 and p == "T531-small.png",
          f"#{m6} {s} preview={p}")

    rc, out = imbo("bob", "send", f"#{g}", "T561 群消息")
    m8 = sent(out)
    s, b = send("alice", {"to": f"#{g}", "body": "T561 群里回", "quote": m8})
    r = read("bob", b.get("message_id")).get("quote", {})
    check(561, 6, "a reply in a group", s == 201 and r.get("from_name") == "bob" and r.get("preview") == "T561 群消息",
          f"#{m8} {s} {r}")

    s, b = send("alice", {"to": f"#{g}", "body": "T561 跨对话", "quote": m1})
    check(561, 7, "a message from another chat is refused", s == 422 and typ(b) == "/errors/reply-not-found"
          and "T561 跨对话" not in inbox_all("bob"), f"{s} {typ(b)}")

    s1, b1 = send("alice", {"to": str(B), "body": "T561 不存在", "quote": 999999999})
    s2, b2 = send("alice", {"to": str(B), "body": "T561 不是数", "quote": "abc"})
    box = inbox_all("bob")
    check(561, 8, "a missing message and a non-number are refused, nothing stored",
          s1 == 422 and typ(b1) == "/errors/reply-not-found" and s2 == 400 and typ(b2) == "/errors/invalid-request"
          and "T561 不存在" not in box and "T561 不是数" not in box, f"{s1} {typ(b1)}; {s2} {typ(b2)}")

    s, b = send("alice", {"to": str(B), "body": "T561 不引用"})
    v = read("bob", b.get("message_id"))
    check(561, 9, "without quote", s == 201 and v.get("body") == "T561 不引用" and "quote" not in v,
          f"{s} keys={sorted(v)}")


def tc562():
    P.up("alice:bob")
    A = P.ids()["alice"]
    S = "55555555-aaaa-bbbb-cccc-000000562001"
    imbo("bob", "dev", "fake-session", S, "--title", "b562")

    out = mcp("bob", "send", {"to": str(A), "body": "T562 帮我看下日志"}, S)
    m1 = sent(out)
    check(562, 1, "the session sends", m1 is not None and "message #" in out, out.strip())

    rc, out = imbo("alice", "send", "bob/562001", "--reply", str(m1), "T562 看过了")
    m2 = sent(out)
    check(562, 2, "imbo send --reply", rc == 0 and m2 is not None and "bob" in out, out)

    out = mcp("bob", "inbox", {}, S)
    check(562, 3, "the session reads which message it answers", "T562 看过了" in out
          and "In reply to bob: T562 帮我看下日志\nT562 看过了" in out, out.strip())

    out = mcp("bob", "send", {"to": str(A), "body": "T562 好的", "quote": m2}, S)
    r = read("alice", sent(out)).get("quote", {})
    check(562, 4, "send with quote", r.get("from_name") == "alice" and r.get("preview") == "T562 看过了",
          f"{out.strip()} {r}")

    out = mcp("bob", "send", {"to": str(A), "body": "T562 不带"}, S)
    v = read("alice", sent(out))
    check(562, 5, "send without quote", v.get("body") == "T562 不带" and "quote" not in v,
          f"{out.strip()} keys={sorted(v)}")

    rc, out = imbo("bob", "send", "--help")
    check(562, 6, "--help lists --reply", "--reply <MESSAGE_ID>" in out,
          next((l.strip() for l in out.splitlines() if "--reply" in l), out))

    rc, out = imbo("alice", "send", "bob", "--reply", "999999999", "T562 找不到")
    check(562, 7, "a missing message", rc == 1 and "Can't find the message to reply to" in out, f"exit {rc}: {out}")


def tc563():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    B, C = ids["bob"], ids["carol"]
    g = P.make_group("t563", [("bob", B), ("carol", C)])
    C1 = "55555555-aaaa-bbbb-cccc-000000563001"
    imbo("carol", "dev", "fake-session", C1, "--title", "c563")
    imbo("carol", "group", "sessions", "t563", "--sessions", "563001")

    rc, out = imbo("bob", "send", f"#{g}", "T563 原话")
    m1 = sent(out)
    s, b, _ = call("POST", "/v1/blocks", {"account_id": B}, bearer=P.app_token("carol@imbo.test"))
    check(563, 1, "carol blocks bob", m1 is not None and s == 201, f"#{m1} {s} {b}")

    rc, out = imbo("alice", "send", f"#{g}", "--reply", str(m1), "T563 回复")
    m2 = sent(out)
    r = read("carol", m2).get("quote")
    check(563, 2, "carol sees only the number", r == {"id": m1}, f"#{m2} {r}")

    r = read("bob", m2).get("quote", {})
    check(563, 3, "bob sees the text", r.get("preview") == "T563 原话", str(r))

    out = mcp("carol", "inbox", {}, C1)
    check(563, 4, "carol's session can't see the original", "T563 回复" in out
          and "In reply to a message you can't see\nT563 回复" in out and "T563 原话" not in out, out.strip())

    rc, out = imbo("carol", "send", f"#{g}", "--reply", str(m1), "T563 我也回")
    check(563, 5, "carol can't reply to it", rc == 1 and "Can't find the message to reply to" in out, f"exit {rc}: {out}")


def tc595():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    A, B, C = ids["alice"], ids["bob"], ids["carol"]
    g = P.make_group("t595", [("bob", B), ("carol", C)])
    S, S2 = "55555555-aaaa-bbbb-cccc-000000595001", "55555555-aaaa-bbbb-cccc-000000595002"
    imbo("bob", "dev", "fake-session", S, "--title", "b595")
    imbo("alice", "dev", "fake-session", S2, "--title", "a595")

    rc, out = imbo("bob", "send", str(A), "T595 今天先发安卓,明天发 iPhone")
    m1 = sent(out)
    check(595, 1, "bob sends", rc == 0 and m1 is not None, out)

    s, b = send("alice", {"to": str(B), "body": "T595 好", "quote": m1, "quote_text": "明天发 iPhone"})
    q = read("bob", b.get("message_id")).get("quote")
    check(595, 2, "a reply to a passage", s == 201 and q == {"id": m1, "from_name": "bob",
          "preview": "T595 今天先发安卓,明天发 iPhone", "text": "明天发 iPhone"}, f"{s} {q}")

    s, b = send("alice", {"to": "bob/595001", "body": "T595 回会话", "quote": m1, "quote_text": "明天发 iPhone"})
    out = mcp("bob", "inbox", {}, S)
    check(595, 3, "the session reads the passage above the body", s == 201
          and "In reply to bob: 明天发 iPhone\nT595 回会话" in out, f"{s} {out.strip()}")

    rc, out = imbo("bob", "send", str(A), "T595 第一行\n第二行")
    m3 = sent(out)
    s, b = send("alice", {"to": str(B), "body": "T595 回两行", "quote": m3, "quote_text": "第一行\n第二行"})
    t = read("bob", b.get("message_id")).get("quote", {}).get("text")
    check(595, 4, "line breaks become spaces", s == 201 and t == "第一行 第二行", f"#{m3} {s} text={t!r}")

    s, b = send("alice", {"to": str(B), "body": "T595 后天", "quote": m1, "quote_text": "后天发"})
    check(595, 5, "a passage not in the message is refused, nothing stored", s == 400
          and typ(b) == "/errors/invalid-request" and "T595 后天" not in inbox_all("bob"), f"{s} {typ(b)}")

    rc, out = imbo("bob", "send", str(A), "字" * 600)
    m5 = sent(out)
    s1, _ = send("alice", {"to": str(B), "body": "T595 五百", "quote": m5, "quote_text": "字" * 500})
    s2, b2 = send("alice", {"to": str(B), "body": "T595 五百零一", "quote": m5, "quote_text": "字" * 501})
    check(595, 6, "500 characters go, 501 are refused", s1 == 201 and s2 == 400
          and typ(b2) == "/errors/invalid-request", f"#{m5} {s1}; {s2} {typ(b2)}")

    s1, b1 = send("alice", {"to": str(B), "body": "T595 只有一段", "quote_text": "明天发 iPhone"})
    s2, b2 = send("alice", {"to": str(B), "body": "T595 空的", "quote": m1, "quote_text": ""})
    box = inbox_all("bob")
    check(595, 7, "without quote, or empty: refused, nothing stored", s1 == 400 and typ(b1) == "/errors/invalid-request"
          and s2 == 400 and typ(b2) == "/errors/invalid-request" and "T595 只有一段" not in box and "T595 空的" not in box,
          f"{s1} {typ(b1)}; {s2} {typ(b2)}")

    rc, out = imbo("bob", "send", f"#{g}", "T595 群里的原话")
    m6 = sent(out)
    TC = P.app_token("carol@imbo.test")
    sb, _, _ = call("POST", "/v1/blocks", {"account_id": B}, bearer=TC)
    s, b = send("alice", {"to": f"#{g}", "body": "T595 群里回", "quote": m6, "quote_text": "原话"})
    sg, v, _ = call("GET", f"/v1/messages/{b.get('message_id')}", bearer=TC)
    q = (v or {}).get("message", {}).get("quote")
    check(595, 8, "carol, who can't see the original, gets no text", sb == 201 and s == 201 and q == {"id": m6},
          f"block {sb}; {s}; {sg} {q}")

    out = mcp("alice", "send", {"to": str(A), "body": "T595 会话说完了"}, S2)
    m8 = sent(out)
    TA = P.app_token("alice@imbo.test")
    _, ag, _ = call("GET", "/v1/agents", bearer=TA)
    sid = next((a["id"] for a in (ag or {}).get("agents", []) if a["title"] == "a595"), None)
    s, b, _ = call("POST", f"/v1/agents/{sid}/messages",
                   {"body": "T595 收到", "client_msg_id": "t595-9", "quote": m8, "quote_text": "说完了"}, bearer=TA)
    m9 = ((b or {}).get("message") or {}).get("id")
    t = read("alice", m9).get("quote", {}).get("text")
    check(595, 9, "to my own session (API-043) with a passage", m8 is not None and s == 201 and t == "说完了",
          f"#{m8} {sid} {s} #{m9} text={t!r}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["561", "562", "563", "595"]
    head = os.environ.get("RUN_HEADER")
    root = os.path.dirname(os.path.dirname(HERE))
    for tc in want:
        OUT.clear()
        if head:
            OUT.append(head.replace("<TC>", f"TC-{tc}"))
        globals()[f"tc{tc}"]()
        with open(f"{root}/docs/test-runs/{run.lower()}-tc-{tc}.txt", "w") as f:
            f.write("\n".join(OUT) + "\n")
    subprocess.run(["sh", ENV, "down"], capture_output=True)
    print(f"failed steps: {P.FAILS}")
    sys.exit(1 if P.FAILS else 0)


if __name__ == "__main__":
    main()
