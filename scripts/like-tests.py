#!/usr/bin/env python3
"""TC-635 (docs/TEST.md): a 👍 on a message from the web app or the App
(FLOW-LIKE, API-091..093, API-031, INV-038), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/like-tests.py <RUN-ID> [635]

Starts its own server and seed the way parity-tests.py does and writes its
lines to docs/test-runs/<run>-tc-635.txt (with RUN_HEADER, if set, on the
first line); exits 1 when any step failed.
"""

import http.client
import importlib.util
import json
import os
import subprocess
import sys

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
conv_with, msgs, by_body = I.conv_with, I.msgs, I.by_body
ENV = os.path.join(HERE, "local-env.sh")
SESSION = "55555555-aaaa-bbbb-cccc-000000635001"


def like(auth, mid, method="PUT"):
    s, b, _ = call(method, f"/v1/messages/{mid}/like", **auth)
    return s, b or {}


def names(likes):
    return [a["name"] for a in likes]


def liked(auth, cid, body):
    """Who has a 👍 on the message with `body`, as API-031 lists it."""
    return names(by_body(msgs(auth, cid), body).get("likes", []))


def as_session(who, method, path):
    """`who`'s session SESSION calling with the computer token: the status."""
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request(method, path, headers={"authorization": f"Bearer {P.computer_token(f'{L}/{who}')}",
                                     "imbo-actor-session": f"claude-code:{SESSION}"})
    r = c.getresponse()
    body = r.read()
    c.close()
    return r.status, (json.loads(body) if body else {})


def tc635():
    P.up("alice:bob,alice:carol,bob:carol")
    i = P.ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    gid = P.make_group("t635", [("bob", B), ("carol", C)])
    imbo("bob", "dev", "fake-session", SESSION, "--title", "b635")
    alice = {"bearer": P.app_token("alice@imbo.test")}
    bob = {"bearer": P.app_token("bob@imbo.test")}
    carol = {"bearer": P.app_token("carol@imbo.test")}
    imbo("bob", "send", f"#{gid}", "T635 群一")
    imbo("alice", "send", str(B), "T635 私聊")
    imbo("alice", "send", str(C), "T635 给 carol")
    g = conv_with(bob, group="t635").get("id")
    m = by_body(msgs(bob, g), "T635 群一").get("id")
    d = by_body(msgs(bob, conv_with(bob, peer="alice").get("id")), "T635 私聊").get("id")
    c = by_body(msgs(carol, conv_with(carol, peer="alice").get("id")), "T635 给 carol").get("id")
    OUT.append(f"前置:群 t635 #{gid},群对话 {g};<m> {m},<d> {d},<c> {c}")

    s, b = like(alice, m)
    check(635, 1, "alice 点", s == 200 and b == {"likes": [{"id": A, "name": "alice"}]}, f"{s} {b}")

    s1, b1 = like(alice, m)
    s2, b2 = like(carol, m)
    check(635, 2, "点过不重记,carol 再点", s1 == 200 and names(b1["likes"]) == ["alice"]
          and s2 == 200 and names(b2["likes"]) == ["alice", "carol"], f"{s1} {b1}; {s2} {b2}")

    seen = liked(bob, g, "T635 群一")
    s1, b1, _ = call("GET", f"/v1/conversations/{g}/likes?after={m - 1}", **bob)
    s2, b2, _ = call("GET", f"/v1/conversations/{g}/likes?after={m}", **bob)
    want = [{"id": m, "likes": [{"id": A, "name": "alice"}, {"id": C, "name": "carol"}]}]
    check(635, 3, "API-031 与 API-093 列出", seen == ["alice", "carol"] and s1 == 200 and b1 == {"messages": want}
          and s2 == 200 and b2 == {"messages": []}, f"likes {seen}; after m-1 {s1} {b1}; after m {s2} {b2}")

    s, b = like(bob, m)
    check(635, 4, "bob 点自己发的", s == 200 and names(b["likes"]) == ["alice", "carol", "bob"], f"{s} {b}")

    s, b, _ = call("POST", "/v1/blocks", {"account_id": A}, **carol)
    by_carol, by_bob = liked(carol, g, "T635 群一"), liked(bob, g, "T635 群一")
    check(635, 5, "carol 屏蔽 alice 后看不到她的 👍", s in (200, 201) and by_carol == ["carol", "bob"]
          and by_bob == ["alice", "carol", "bob"], f"屏蔽 {s};carol 看 {by_carol};bob 看 {by_bob}")

    s1, b1 = like(alice, m, "DELETE")
    s2, b2 = like(alice, m, "DELETE")
    check(635, 6, "alice 取消两次", s1 == 200 and s2 == 200 and names(b1["likes"]) == ["carol", "bob"]
          and names(b2["likes"]) == ["carol", "bob"], f"{s1} {b1}; {s2} {b2}")

    got = [(s, typ(b)) for s, b in (like(carol, d), like(bob, c), like(alice, 99999999))]
    check(635, 7, "别人的对话、不存在的消息", got == [(404, "/errors/not-found")] * 3, got)

    imbo("alice", "send", f"#{gid}", "T635 群二")
    m2 = by_body(msgs(bob, g), "T635 群二").get("id")
    s, b = like(carol, m2)
    check(635, 8, "carol 点她屏蔽的 alice 发的", s == 404 and typ(b) == "/errors/not-found", f"<m2> {m2}: {s} {b}")

    s1, b1, _ = call("PUT", f"/v1/messages/{d}/like", bearer=P.computer_token(f"{L}/alice"))
    s2, b2 = as_session("bob", "PUT", f"/v1/messages/{d}/like")
    check(635, 9, "终端与会话点不了", (s1, typ(b1), s2, typ(b2)) == (403, "/errors/forbidden", 403, "/errors/forbidden"),
          f"终端 {s1} {b1}; 会话 {s2} {b2}")

    rows = msgs(alice, g)
    shown = [(x["body"], names(x.get("likes", []))) for x in rows]
    check(635, 10, "没多出消息", shown == [("T635 群一", ["carol", "bob"]), ("T635 群二", [])], shown)


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["635"]
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
