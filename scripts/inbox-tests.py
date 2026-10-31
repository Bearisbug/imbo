#!/usr/bin/env python3
"""TC-377, TC-378 and TC-379 (docs/TEST.md): what the red dot in the web
app and the App counts and marking a conversation read (API-030, API-031,
API-078), where a message sent in person was written from (ADR-056), and
an @ in a group naming a member (RULE-015, ADR-057), against the local
server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/inbox-tests.py <RUN-ID> [377 378 379]

Each case starts its own server and seed the way parity-tests.py does, and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed. About a minute.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, M, OUT, call, check, imbo, typ = P.L, os.environ["M"], P.OUT, P.call, P.check, P.imbo, P.typ
SID = "55555555-aaaa-bbbb-cccc-000000"


def fake(who, sid, title, *more):
    return imbo(who, "dev", "fake-session", f"{SID}{sid}", "--title", title, *more)


def mcp(who, tool, args, sid):
    r = subprocess.run(["python3", M, f"{L}/{who}", tool, json.dumps(args), "--session", f"claude-code:{SID}{sid}"],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


def convs(auth):
    _, b, _ = call("GET", "/v1/conversations", **auth)
    return b["conversations"]


def conv_with(auth, peer=None, group=None):
    for c in convs(auth):
        if peer and (c.get("peer") or {}).get("name") == peer or group and (c.get("group") or {}).get("name") == group:
            return c
    return {}


def msgs(auth, cid):
    _, b, _ = call("GET", f"/v1/conversations/{cid}/messages", **auth)
    return b["messages"]


def by_body(messages, body):
    return next((m for m in messages if m["body"] == body), {})


def lines_with(out, tag):
    return [l.strip() for l in out.splitlines() if tag in l]


def entries(out):
    """`imbo inbox` messages as (header line, first body line): the state
    ends the header, the body follows on the next line."""
    rows = out.splitlines()
    return [(l.strip(), rows[k + 1].strip() if k + 1 < len(rows) else "")
            for k, l in enumerate(rows) if l.strip().startswith("#")]


def tc377():
    P.up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=P.ORIGIN)
    i = P.ids()
    fake("bob", "377001", "t377")
    g = P.make_group("t377g", [("bob", i["bob"])])
    imbo("bob", "group", "sessions", "t377g", "--sessions", "377001")
    fake("bob", "377001", "t377", "--remove")
    bob = {"bearer": P.app_token("bob@imbo.test")}
    carol = {"cookie": P.web_cookie("carol@imbo.test")}
    computer = {"bearer": P.computer_token(f"{L}/bob")}
    OUT.append(f"前置:群 t377g #{g}")

    imbo("alice", "send", str(i["bob"]), "T377-1")
    imbo("alice", "send", str(i["bob"]), "T377-2")
    c = conv_with(bob, peer="alice")
    C = c.get("id")
    check(377, 1, "两条发进收件箱", c.get("pending") == 2, f"与 alice 的私聊 pending={c.get('pending')}")

    imbo("alice", "send", "bob/t377", "T377-3")
    imbo("alice", "send", f"#{g}", "T377-4")
    c, gc = conv_with(bob, peer="alice"), conv_with(bob, group="t377g")
    check(377, 2, "发给没上线的会话不算", c.get("pending") == 2 and gc.get("pending") == 0,
          f"私聊 pending={c.get('pending')};群 t377g pending={gc.get('pending')}")

    dm, gm = msgs(bob, C), msgs(bob, gc.get("id"))
    st = {b: by_body(dm, b).get("state") for b in ("T377-1", "T377-2", "T377-3")}
    st["T377-4"] = by_body(gm, "T377-4").get("state")
    check(377, 3, "没送进会话的带 state",
          st == {"T377-1": None, "T377-2": None, "T377-3": "pending", "T377-4": "pending"}, f"state {st}")

    up_to = by_body(dm, "T377-2").get("id")
    s, b, _ = call("POST", f"/v1/conversations/{C}/read", {"up_to": up_to}, **bob)
    check(377, 4, "App 记已读", s == 200 and (b or {}).get("marked") == 2, f"{s} {b}")
    c = conv_with(bob, peer="alice")
    check(377, 5, "红点归零", c.get("pending") == 0, f"pending={c.get('pending')}")
    s, b, _ = call("POST", f"/v1/conversations/{C}/read", {"up_to": up_to}, **bob)
    check(377, 6, "再记一次", s == 200 and (b or {}).get("marked") == 0, f"{s} {b}")

    _, new = imbo("bob", "inbox")
    _, every = imbo("bob", "inbox", "--all")
    heads = [l for l in new.splitlines() if l.startswith("Inbox") or l.startswith("Terminal inbox")]
    OUT.append(f"bob imbo inbox:\n{new}\nbob imbo inbox --all:\n{every}")
    waiting = [b for h, b in entries(new) if h.endswith("waiting") and b in ("T377-3", "T377-4")]
    read = [b for h, b in entries(every) if h.endswith("read") and b in ("T377-1", "T377-2")]
    ok = (not heads and "T377-1" not in new and "T377-2" not in new and len(waiting) == 2
          and "Inbox" in every.splitlines()[0] and len(read) == 2)
    check(377, 7, "imbo inbox 不再列出", ok,
          f"inbox 标题 {heads};T377-1/2 在 inbox 里:{'是' if 'T377-1' in new or 'T377-2' in new else '否'};"
          f"waiting {len(waiting)} 条;--all 首行 {every.splitlines()[0] if every else ''!r},标 read {len(read)} 条")

    imbo("alice", "send", str(i["carol"]), "T377-5")
    c = conv_with(carol, peer="alice")
    before = c.get("pending")
    last = by_body(msgs(carol, c.get("id")), "T377-5").get("id")
    s, b, _ = call("POST", f"/v1/conversations/{c.get('id')}/read", {"up_to": last}, **carol)
    after = conv_with(carol, peer="alice").get("pending")
    check(377, 8, "网页记已读", before == 1 and s == 200 and (b or {}).get("marked") == 1 and after == 0,
          f"之前 pending={before};记已读 {s} {b};之后 pending={after}")

    s, b, _ = call("POST", f"/v1/conversations/{C}/read", {"up_to": up_to}, **computer)
    check(377, 9, "电脑登录被拒", s == 403 and typ(b) == "/errors/forbidden", f"{s} {typ(b)}")

    s1, b1, _ = call("POST", f"/v1/conversations/{C}/read", {}, **bob)
    s2, b2, _ = call("POST", f"/v1/conversations/{C}/read", {"up_to": 1}, **carol)
    check(377, 10, "缺 up_to 与别人的对话",
          s1 == 400 and typ(b1) == "/errors/invalid-request" and s2 == 404 and typ(b2) == "/errors/not-found",
          f"缺 up_to:{s1} {typ(b1)};carol 对 <C>:{s2} {typ(b2)}")


def tc378():
    P.up("alice:bob", IMBO_WEB_ORIGIN=P.ORIGIN)
    i = P.ids()
    A = i["alice"]
    fake("bob", "378001", "t378")
    fake("alice", "378002", "a378")
    alice = {"bearer": P.app_token("alice@imbo.test")}
    bob = {"cookie": P.web_cookie("bob@imbo.test")}

    code, _ = imbo("alice", "send", "bob/t378", "T378-cli")
    s, b, _ = call("POST", "/v1/messages", {"to": "bob/t378", "body": "T378-app", "client_msg_id": "t378-app"}, **alice)
    check(378, 1, "终端与 App 发出", code == 0 and s == 201, f"命令退出码 {code};App {s}")
    s, b, _ = call("POST", "/v1/messages", {"to": "alice/a378", "body": "T378-web", "client_msg_id": "t378-web"}, **bob)
    check(378, 2, "网页发出", s == 201, f"{s} {typ(b) or ''}")

    out = mcp("bob", "inbox", {}, "378001")
    want = "the reply goes to their inbox"
    ok = ("alice · terminal" in out and "alice · App" in out and out.count(want) >= 2
          and "terminal inbox" not in out)
    check(378, 3, "会话看到终端与 App", ok,
          f"alice · terminal:{'有' if 'alice · terminal' in out else '没有'};alice · App:{'有' if 'alice · App' in out else '没有'};"
          f"「{want}」{out.count(want)} 次;terminal inbox:{'有' if 'terminal inbox' in out else '没有'}")
    out = mcp("alice", "inbox", {}, "378002")
    check(378, 4, "会话看到网页", "bob · Web" in out, f"T378-web 一行:{lines_with(out, 'bob ·')[:1]}")

    c = conv_with(bob, peer="alice")
    dm = msgs(bob, c.get("id"))
    via = {t: by_body(dm, t).get("from", {}).get("via") for t in ("T378-cli", "T378-app", "T378-web")}
    agents = [t for t in via if by_body(dm, t).get("from", {}).get("agent")]
    check(378, 5, "对话接口的来源", via == {"T378-cli": "terminal", "T378-app": "app", "T378-web": "web"} and not agents,
          f"from.via {via};带 from.agent 的 {agents}")

    _, ag, _ = call("GET", "/v1/agents", **alice)
    G = next((a["id"] for a in ag["agents"] if a["title"] == "a378"), None)
    s, b, _ = call("POST", f"/v1/agents/{G}/messages", {"body": "T378-self", "client_msg_id": "t378-self"}, **alice)
    v = ((b or {}).get("message") or {}).get("from", {}).get("via")
    check(378, 6, "App 发给自己的会话", s == 201 and v == "app", f"{s} from.via={v}")

    out = mcp("alice", "inbox", {}, "378002")
    want = f"Reply to: {A} (from your own user in person; the reply goes to their inbox)"
    check(378, 7, "自己的会话看到 App", "alice · App" in out and want in out and "T378-web" not in out,
          f"alice · App:{'有' if 'alice · App' in out else '没有'};「{want}」:{'有' if want in out else '没有'}")

    out = mcp("alice", "send", {"to": str(A), "body": "T378 done"}, "378002")
    check(378, 8, "回复进收件箱", "went to alice's inbox" in out, out.strip().splitlines()[0] if out.strip() else "无输出")

    call("POST", "/v1/messages", {"to": str(A), "body": "T378-in", "client_msg_id": "t378-in"}, **bob)
    _, out = imbo("alice", "inbox")
    first = out.splitlines()[0] if out else ""
    done, came = lines_with(out, "a378"), lines_with(out, "bob · Web")
    check(378, 9, "imbo inbox 的标题与发件人", first.startswith("Inbox") and "2 new" in first and done and came,
          f"首行 {first!r};alice · a378 {len(done)} 行;bob · Web {len(came)} 行")

    _, out = imbo("bob", "send", str(A), "T378-cli2")
    check(378, 10, "发送提示", "to alice's inbox" in out, out.splitlines()[0] if out else "")


def got(token, body):
    """Short IDs of what got `body` in this account's GET /v1/inbox, the
    terminal inbox written as "inbox"."""
    _, b, _ = call("GET", "/v1/inbox?all=true", bearer=token)
    return sorted("inbox" if it["agent"]["tool"] == "terminal" else it["agent"]["short_id"]
                  for it in b["items"] if it["message"]["body"] == body)


def pushes(body):
    """(account ID, title, body) of each push_logged line for `body`."""
    out = []
    for l in open(P.LOG, errors="replace"):
        if 'event="push_logged"' not in l or body not in l:
            continue
        f = dict(re.findall(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)', l))
        out.append((int(f["account_id"]), json.loads(f["title"]), json.loads(f["body"])))
    return out


def tc379():
    P.up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=P.ORIGIN, IMBO_PUSH_LOG_ONLY="1")
    i = P.ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    g = P.make_group("t379", [("bob", B), ("carol", C)])
    for who, n, title in (("alice", "379001", "front"), ("bob", "379002", "api"), ("carol", "379003", "docs")):
        fake(who, n, title)
        imbo(who, "group", "sessions", "t379", "--sessions", n)
    tok = {w: P.computer_token(f"{L}/{w}") for w in ("alice", "bob", "carol")}
    bob = {"bearer": P.app_token("bob@imbo.test")}
    carol = {"bearer": P.app_token("carol@imbo.test")}
    call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t379-bob]", "locale": "zh-Hans"}, **bob)
    call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t379-carol]"}, **carol)
    OUT.append(f"前置:群 t379 #{g};alice {A},bob {B},carol {C}")

    def send(body):
        out = mcp("alice", "send", {"to": f"#{g}", "body": body}, "379001").strip()
        return out.splitlines()[0] if out else ""

    def where(body):
        return {w: got(tok[w], body) for w in tok}

    out = send("@bob 帮忙看下 T379-1")
    want = f"Sent to group t379 (#{g}): message #"
    check(379, 1, "@ 一个人", out.startswith(want) and out.endswith(", 0 receiving session(s), and to the inbox of bob."), out)
    w = where("@bob 帮忙看下 T379-1")
    check(379, 2, "只进他的收件箱", w == {"alice": [], "bob": ["inbox"], "carol": []}, f"收到的 {w}")
    gc = conv_with(bob, group="t379")
    check(379, 3, "算进红点", gc.get("pending") == 1, f"群 t379 pending={gc.get('pending')}")
    time.sleep(1)
    p = pushes("T379-1")
    to_bob = [x for x in p if x[0] == B]
    to_carol = [x for x in p if x[0] == C]
    ok = (to_bob == [(B, "t379", "alice · front @了你：@bob 帮忙看下 T379-1")]
          and to_carol == [(C, "t379", "alice · front: @bob 帮忙看下 T379-1")])
    check(379, 4, "推送", ok, f"推送记录 {p}")

    m1 = by_body(msgs(bob, gc.get("id")), "@bob 帮忙看下 T379-1").get("id")
    s, b, _ = call("POST", f"/v1/conversations/{gc.get('id')}/read", {"up_to": m1}, **bob)
    after = conv_with(bob, group="t379").get("pending")
    check(379, 5, "记已读", s == 200 and (b or {}).get("marked") == 1 and after == 0, f"{s} {b};之后 pending={after}")

    out = send("@Bob @api T379-2")
    w = where("@Bob @api T379-2")
    check(379, 6, "人和会话都点", "1 receiving session(s), and to the inbox of bob" in out
          and w == {"alice": [], "bob": ["379002", "inbox"], "carol": []}, f"{out};收到的 {w}")

    _, out = imbo("alice", "send", f"#{g}", "@alice T379-3")
    w = where("@alice T379-3")
    check(379, 7, "亲手 @ 自己不算", w == {"alice": ["379001"], "bob": ["379002"], "carol": ["379003"]},
          f"{out.splitlines()[0] if out else ''};收到的 {w}")

    out = send("@alice 做完了 T379-4")
    w = where("@alice 做完了 T379-4")
    check(379, 8, "会话 @ 自己的账号", "0 receiving session(s), and to the inbox of alice" in out
          and w == {"alice": ["inbox"], "bob": [], "carol": []}, f"{out};收到的 {w}")

    fake("carol", "379004", "bob")
    imbo("carol", "group", "sessions", "t379", "--sessions", "379003,379004")
    send("@bob @carol T379-5")
    w = where("@bob @carol T379-5")
    time.sleep(1)
    pc = [x for x in pushes("T379-5") if x[0] == C]
    check(379, 9, "同名都收、没登记语言写英文",
          w == {"alice": [], "bob": ["inbox"], "carol": ["379004", "inbox"]}
          and pc == [(C, "t379", "alice · front mentioned you: @bob @carol T379-5")], f"收到的 {w};推给 carol {pc}")

    send(f"@{B} T379-6")
    w = where(f"@{B} T379-6")
    check(379, 10, "@ 账号 ID", w == {"alice": [], "bob": ["inbox"], "carol": []}, f"收到的 {w}")

    call("POST", "/v1/blocks", {"account_id": A}, **carol)
    out = send("@carol T379-7")
    w = where("@carol T379-7")
    check(379, 11, "屏蔽了发件人的点不到", out.startswith(want) and out.endswith(", 1 receiving session(s).")
          and w == {"alice": [], "bob": ["379002"], "carol": []}, f"{out};收到的 {w}")

    s, b, _ = call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t379-bob]", "locale": "fr"}, **bob)
    check(379, 12, "语言不认", s == 400 and typ(b) == "/errors/invalid-request", f"{s} {typ(b)}")

    _, b, _ = call("GET", "/v1/groups", **bob)
    grp = next((x for x in b["groups"] if x["id"] == g), {})
    ses = {m["name"]: sorted(x["title"] for x in m.get("sessions", [])) for m in grp.get("members", [])}
    keys = all(set(x) >= {"id", "short_id", "title", "tool"} for m in grp.get("members", []) for x in m.get("sessions", []))
    check(379, 13, "成员带会话", ses == {"alice": ["front"], "bob": ["api"], "carol": ["bob", "docs"]} and keys,
          f"成员的会话 {ses};字段齐全:{'是' if keys else '否'}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["377", "378", "379"]
    head = os.environ.get("RUN_HEADER")
    root = os.path.dirname(os.path.dirname(HERE))
    for tc in want:
        OUT.clear()
        if head:
            OUT.append(head.replace("<TC>", f"TC-{tc}"))
        try:
            globals()[f"tc{tc}"]()
        except Exception as e:  # a step the build can't take yet still leaves a record
            check(int(tc), 0, "中断", False, f"{type(e).__name__}: {e}")
        with open(f"{root}/docs/test-runs/{run.lower()}-tc-{tc}.txt", "w") as f:
            f.write("\n".join(OUT) + "\n")
    subprocess.run(["sh", P.ENV, "down"], capture_output=True)
    print(f"failed steps: {P.FAILS}")
    sys.exit(1 if P.FAILS else 0)


if __name__ == "__main__":
    main()
