#!/usr/bin/env python3
"""TC-566 and TC-568 (docs/TEST.md): pinning, marking unread and muting a
conversation (FLOW-CHAT-PREFS, API-085, API-030, API-078, INV-037), and
finding messages in one conversation (FLOW-CHAT-SEARCH, API-086, INV-036),
against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/prefs-tests.py <RUN-ID> [566 568]

Each case starts its own server and seed the way parity-tests.py does and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed. About two minutes:
TC-568 waits out the 30-a-minute limit once.
"""

import importlib.util
import os
import subprocess
import sys
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


P = load("parity", "parity-tests.py")
I = load("inbox", "inbox-tests.py")
L, OUT, call, check, imbo, typ = P.L, P.OUT, P.call, P.check, P.imbo, P.typ
convs, conv_with, msgs, pushes, got, fake = I.convs, I.conv_with, I.msgs, I.pushes, I.got, I.fake
ENV = os.path.join(HERE, "local-env.sh")


def prefs(auth, cid, body):
    s, b, _ = call("POST", f"/v1/conversations/{cid}/prefs", body, **auth)
    return s, b or {}


def marks(c):
    """The three settings as API-030 lists them; a missing one is false."""
    return {k: c[k] for k in ("pinned", "marked_unread", "muted") if k in c}


def search(auth, cid, q):
    s, b, _ = call("GET", f"/v1/conversations/{cid}/search?q={urllib.parse.quote(q)}", **auth)
    return s, b or {}


def bodies(b):
    return [m["body"] for m in b.get("messages", [])]


def tc566():
    P.up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=P.ORIGIN, IMBO_PUSH_LOG_ONLY="1")
    i = P.ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    g = P.make_group("t566", [("bob", B), ("carol", C)])
    fake("bob", "566001", "b566")
    imbo("bob", "group", "sessions", "t566", "--sessions", "566001")
    bob = {"bearer": P.app_token("bob@imbo.test")}
    carol = {"bearer": P.app_token("carol@imbo.test")}
    call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t566-bob]", "locale": "zh-Hans"}, **bob)
    call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t566-carol]"}, **carol)
    imbo("alice", "send", str(C), "T566 给 carol")
    imbo("alice", "send", str(B), "T566 私聊一")
    imbo("alice", "send", f"#{g}", "T566 群一")
    dm, gc = conv_with(bob, peer="alice").get("id"), conv_with(bob, group="t566").get("id")
    other = conv_with({"bearer": P.computer_token(f"{L}/alice")}, peer="carol").get("id")
    OUT.append(f"前置:群 t566 #{g};bob 的私聊 {dm}、群对话 {gc};alice 与 carol 的私聊 {other}")

    s, b = prefs(bob, dm, {"pinned": True})
    order = [(c["id"], marks(c)) for c in convs(bob)]
    check(566, 1, "置顶的排最前", s == 200 and b == {"pinned": True, "marked_unread": False, "muted": False}
          and order[:2] == [(dm, {"pinned": True}), (gc, {})], f"{s} {b};列表 {order}")

    alice = conv_with({"bearer": P.computer_token(f"{L}/alice")}, peer="bob")
    check(566, 2, "对方看不出", alice.get("id") == dm and marks(alice) == {}, f"alice 那边 {marks(alice)}")

    s, b = prefs(bob, gc, {"marked_unread": True})
    on = marks(conv_with(bob, group="t566"))
    last = msgs(bob, gc)[-1]["id"]
    s2, b2, _ = call("POST", f"/v1/conversations/{gc}/read", {"up_to": last}, **bob)
    off = marks(conv_with(bob, group="t566"))
    check(566, 3, "标为未读，标为已读去掉它", s == 200 and b.get("marked_unread") is True and on == {"marked_unread": True}
          and s2 == 200 and off == {}, f"{s} 之后 {on};记已读 {s2} 之后 {off}")

    prefs(bob, gc, {"muted": True})
    imbo("alice", "send", f"#{g}", "T566 群二")
    time.sleep(1)
    p = [x[0] for x in pushes("T566 群二")]
    w = got(P.computer_token(f"{L}/bob"), "T566 群二")
    check(566, 4, "免打扰的群不推，会话照收", B not in p and C in p and w == ["566001"], f"推给 {p};bob 收到的 {w}")

    imbo("alice", "send", f"#{g}", "@bob T566 群三")
    time.sleep(1)
    p = [x for x in pushes("T566 群三") if x[0] == B]
    check(566, 5, "@ 了本人照样推", len(p) == 1 and "@了你" in p[0][2], f"推给 bob {p}")

    prefs(bob, dm, {"muted": True})
    imbo("alice", "send", str(B), "T566 私聊二")
    time.sleep(1)
    before, after = [x[0] for x in pushes("T566 私聊一")], [x[0] for x in pushes("T566 私聊二")]
    c = conv_with(bob, peer="alice")
    check(566, 6, "免打扰的私聊不推，条数照加", B in before and B not in after and c.get("pending") == 2,
          f"私聊一推给 {before};私聊二推给 {after};pending={c.get('pending')}")

    s1, b1 = prefs(bob, dm, {})
    s2, b2 = prefs(bob, dm, {"pinned": "yes"})
    s3, b3 = prefs(bob, other, {"pinned": True})
    s4, b4 = prefs({"bearer": P.computer_token(f"{L}/bob")}, dm, {"pinned": False})
    kept = marks(conv_with(bob, peer="alice"))
    check(566, 7, "改不了的都不记", (s1, typ(b1), s2, typ(b2), s3, typ(b3), s4, typ(b4))
          == (400, "/errors/invalid-request", 400, "/errors/invalid-request", 404, "/errors/not-found", 403, "/errors/forbidden")
          and kept == {"pinned": True, "muted": True},
          f"空 {s1} {typ(b1)};非真假 {s2} {typ(b2)};别人的对话 {s3} {typ(b3)};终端 {s4} {typ(b4)};之后 {kept}")

    imbo("alice", "send", f"#{g}", "T566 群四")
    pinned = [x["id"] for x in convs(bob)][:2]
    s, b = prefs(bob, dm, {"pinned": False})
    order = [x["id"] for x in convs(bob)][:2]
    check(566, 8, "取消置顶，回到按最后一条排", pinned == [dm, gc] and s == 200
          and b == {"pinned": False, "marked_unread": False, "muted": True} and order == [gc, dm],
          f"取消前 {pinned};{s} {b};取消后 {order}")


def tc568():
    P.up("alice:bob,alice:carol")
    i = P.ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    g = P.make_group("t568", [("bob", B), ("carol", C)])
    bob = {"bearer": P.app_token("bob@imbo.test")}
    carol = {"bearer": P.app_token("carol@imbo.test")}
    imbo("alice", "send", str(C), "T568 给 carol")
    for body in ("T568 Deploy 上线", "T568 deploy 回滚", "T568 进度 100% 了", "T568 a_b 写法", "T568 axb 写法", "T568 别的"):
        imbo("alice", "send", str(B), body)
    imbo("alice", "send", f"#{g}", "T568 needle 一")
    imbo("bob", "send", f"#{g}", "T568 needle 二")
    dm, gc = conv_with(bob, peer="alice").get("id"), conv_with(bob, group="t568").get("id")
    other = conv_with({"bearer": P.computer_token(f"{L}/alice")}, peer="carol").get("id")
    OUT.append(f"前置:群 t568 #{g};bob 的私聊 {dm}、群对话 {gc};alice 与 carol 的私聊 {other}")

    s, b = search(bob, dm, "DEPLOY")
    check(568, 1, "不分大小写，新的在前", s == 200 and bodies(b) == ["T568 deploy 回滚", "T568 Deploy 上线"]
          and b.get("has_more") is False, f"{s} {bodies(b)} has_more={b.get('has_more')}")

    s1, b1 = search(bob, dm, "%")
    s2, b2 = search(bob, dm, "a_b")
    check(568, 2, "% 与 _ 按字面找", s1 == 200 and bodies(b1) == ["T568 进度 100% 了"]
          and s2 == 200 and bodies(b2) == ["T568 a_b 写法"], f"% → {bodies(b1)};a_b → {bodies(b2)}")

    call("POST", "/v1/blocks", {"account_id": A}, **carol)
    s1, b1 = search(carol, gc, "needle")
    s2, b2 = search(bob, gc, "needle")
    check(568, 3, "群里不列本人屏蔽的人发的", s1 == 200 and bodies(b1) == ["T568 needle 二"]
          and bodies(b2) == ["T568 needle 二", "T568 needle 一"], f"carol 查到 {bodies(b1)};bob 查到 {bodies(b2)}")

    # 30 a minute in one conversation: the six above, 24 more, a minute's wait, then 27.
    for n in range(1, 52):
        if n == 25:
            time.sleep(61)
        rc, out = imbo("alice", "send", str(B), f"T568 many {n}")
        if rc:
            OUT.append(f"T568 many {n} 没发出: {out}")
    s, b = search(bob, dm, "many")
    got_ = bodies(b)
    check(568, 4, "超过 50 条只回最新的 50 条", s == 200 and len(got_) == 50 and got_[0] == "T568 many 51"
          and got_[-1] == "T568 many 2" and b.get("has_more") is True, f"{s} {len(got_)} 条,{got_[:1]}…{got_[-1:]} has_more={b.get('has_more')}")

    cases = [("没写字", bob, dm, ""), ("只有空格", bob, dm, "   "), ("101 个字", bob, dm, "字" * 101),
             ("别人的对话", bob, other, "T568"), ("终端", {"bearer": P.computer_token(f"{L}/bob")}, dm, "T568")]
    seen = [(what, *(lambda r: (r[0], typ(r[1])))(search(auth, cid, q))) for what, auth, cid, q in cases]
    want = [(400, "/errors/invalid-request")] * 3 + [(404, "/errors/not-found"), (403, "/errors/forbidden")]
    check(568, 5, "查不了的", [x[1:] for x in seen] == want, "; ".join(f"{w} {st} {ty}" for w, st, ty in seen))

    s, b = search(bob, dm, "字" * 100)
    check(568, 6, "100 个字可以查", s == 200 and bodies(b) == [], f"{s} {bodies(b)}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["566", "568"]
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
