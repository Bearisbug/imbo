#!/usr/bin/env python3
"""TC-591 and TC-592 (docs/TEST.md): what a notification carries for the App
to show the sender and reply on it, and that a reply written to its `to`
lands where it should (FLOW-APP-PUSH S6, FLOW-PUSH-REPLY, ADR-064), against
the local server with IMBO_PUSH_LOG_ONLY.

    . crates/scripts/local-env.sh
    python3 crates/scripts/push-tests.py <RUN-ID> [591 592]

TC-592 goes on from TC-591's data, so 592 runs 591 first. Writes its lines
to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if set, on the first
line); exits 1 when any step failed.
"""

import http.client
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
L, M, OUT, call, check, imbo = P.L, os.environ["M"], P.OUT, P.call, P.check, P.imbo
ENV = os.path.join(HERE, "local-env.sh")
PNG = os.path.expanduser("~/.cache/imbo-test/media/T531-small.png")
FIELD = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')
JSON = json.JSONDecoder()
S_BOB = "55555555-aaaa-bbbb-cccc-000000591001"
S_ALICE = "55555555-aaaa-bbbb-cccc-000000591002"
D = {}  # TC-591's data, for TC-592


def pushes(account, marker):
    """The push_logged lines for `account` whose body has `marker`, waiting up to 6 s."""
    for _ in range(30):
        out = []
        for line in open(P.LOG, errors="replace"):
            if 'event="push_logged"' in line and f"account_id={account}" in line and marker in line:
                f = dict(FIELD.findall(line))
                # data and expo are JSON written as is, spaces and all.
                data, expo = (JSON.raw_decode(line, line.index(f" {k}=") + len(k) + 2)[0] for k in ("data", "expo"))
                out.append({"body": json.loads(f["body"]), "data": data, "expo": expo})
        if out:
            return out
        time.sleep(0.2)
    return []


def mcp(who, args, sid):
    r = subprocess.run(["python3", M, f"{L}/{who}", "send", json.dumps(args), "--session", f"claude-code:{sid}"],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


def put_avatar(token):
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request("PUT", "/v1/me/avatar", body=open(PNG, "rb").read(),
              headers={"authorization": f"Bearer {token}", "content-type": "image/png"})
    r = c.getresponse()
    b = json.loads(r.read() or b"{}")
    c.close()
    return r.status, b.get("avatar_url")


def tc591():
    P.up("alice:bob,alice:carol", IMBO_PUSH_LOG_ONLY="1")
    ids = P.ids()
    A, B, C = ids["alice"], ids["bob"], ids["carol"]
    D.update(A=A, B=B, C=C)
    g = P.make_group("t591", [("bob", B)])
    D["g"] = g
    TA = P.app_token("alice@imbo.test")
    D["TA"] = TA
    s, _, _ = call("PUT", "/v1/app/push", {"push_token": "ExponentPushToken[t591-a]"}, bearer=TA)
    D["TB"] = P.app_token("bob@imbo.test")
    st, url = put_avatar(D["TB"])
    imbo("bob", "dev", "fake-session", S_BOB, "--title", "b591")
    imbo("alice", "dev", "fake-session", S_ALICE, "--title", "a591")
    check(591, 0, "前置: alice's phone registered, bob has a picture", s == 204 and st == 200 and url, f"{s} {st} {url}")

    imbo("bob", "send", str(A), "T591 私聊")
    p = pushes(A, "T591 私聊")
    d, e = (p[0]["data"], p[0]["expo"]) if p else ({}, {})
    D["dm"] = d.get("conversation_id")
    check(591, 1, "bob writes alice: sender with picture, to bob's inbox, reply category, thread = the chat",
          len(p) == 1 and d.get("sender") == {"id": B, "name": "bob", "avatar_url": url} and d.get("to") == str(B)
          and "group" not in d and "agent_id" not in d and e.get("categoryId") == "message"
          and e.get("mutableContent") is True and e.get("threadId") == str(d.get("conversation_id"))
          and e.get("data") == d, json.dumps(p, ensure_ascii=False))

    mcp("bob", {"to": str(A), "body": "T591 会话"}, S_BOB)
    p = pushes(A, "T591 会话")
    d, e = (p[0]["data"], p[0]["expo"]) if p else ({}, {})
    D["to_session"] = d.get("to")
    check(591, 2, "bob's session writes alice: to is bob/<its short ID>, same thread as step 1",
          len(p) == 1 and d.get("to") == f"{B}/{S_BOB[-6:]}" and d.get("sender", {}).get("id") == B
          and e.get("threadId") == str(D["dm"]), json.dumps(p, ensure_ascii=False))

    imbo("bob", "send", f"#{g}", "T591 群")
    p = pushes(A, "T591 群")
    d, e = (p[0]["data"], p[0]["expo"]) if p else ({}, {})
    D["gc"] = d.get("conversation_id")
    check(591, 3, "bob writes the group: to #group, the group without a picture, its own thread",
          len(p) == 1 and d.get("to") == f"#{g}" and d.get("group") == {"id": g, "name": "t591"}
          and d.get("sender", {}).get("id") == B and e.get("threadId") == str(d.get("conversation_id"))
          and d.get("conversation_id") != D["dm"], json.dumps(p, ensure_ascii=False))

    imbo("carol", "send", str(A), "T591 无头像")
    p = pushes(A, "T591 无头像")
    d = p[0]["data"] if p else {}
    check(591, 4, "carol has no picture: no avatar_url", len(p) == 1 and d.get("sender") == {"id": C, "name": "carol"},
          json.dumps(p, ensure_ascii=False))

    mcp("alice", {"to": str(A), "body": "T591 自己的会话"}, S_ALICE)
    p = pushes(A, "T591 自己的会话")
    d = p[0]["data"] if p else {}
    D["agent_id"] = d.get("agent_id")
    check(591, 5, "alice's own session: agent_id, no to", len(p) == 1 and d.get("agent_id") and "to" not in d
          and d.get("sender", {}).get("id") == A, json.dumps(p, ensure_ascii=False))


def last(token, conv):
    s, b, _ = call("GET", f"/v1/conversations/{conv}/messages", bearer=token)
    return (b or {}).get("messages", [])


def tc592():
    if not D:
        tc591()
    A, B, TA, TB = D["A"], D["B"], D["TA"], D["TB"]

    def reply(to, body, cid):
        return call("POST", "/v1/messages", {"to": to, "body": body, "client_msg_id": cid}, bearer=TA)[:2]

    s, b = reply(str(B), "T592 回 bob", "t592-1")
    box = imbo("bob", "inbox", "--all")[1]
    mine = [m for m in last(TA, D["dm"]) if m["body"] == "T592 回 bob"]
    check(592, 1, "to = bob's inbox: lands there, written from the App, no push back to alice",
          s == 201 and b.get("terminal") is True and "T592 回 bob" in box and len(mine) == 1
          and mine[0]["from"].get("via") == "app" and not pushes(A, "T592 回 bob"), f"{s} {b} via={mine and mine[0]['from']}")

    s, b = reply(D["to_session"], "T592 回会话", "t592-2")
    check(592, 2, "to = bob/<short ID>: goes to that session", s == 201 and b.get("recipients") == 1
          and b.get("terminal") is False and b.get("conversation", {}).get("id") == D["dm"], f"{s} {b}")

    s, b = reply(f"#{D['g']}", "T592 回群", "t592-3")
    seen = [m for m in last(TB, D["gc"]) if m["body"] == "T592 回群"]
    check(592, 3, "to = #group: in the group, bob sees it", s == 201 and b.get("conversation", {}).get("id") == D["gc"]
          and len(seen) == 1, f"{s} {b}")

    s, b, _ = call("POST", f"/v1/agents/{D['agent_id']}/messages",
                   {"body": "T592 回自己的会话", "client_msg_id": "t592-4"}, bearer=TA)
    check(592, 4, "agent_id: API-043 to alice's own session, no push back", s == 201
          and not pushes(A, "T592 回自己的会话"), f"{s} {b}")

    s1, b1 = reply(str(B), "T592 回 bob", "t592-1")
    box = imbo("bob", "inbox", "--all")[1]
    check(592, 5, "the same reply handed over twice: stored once", s1 == 201 and box.count("T592 回 bob") == 1,
          f"{s1} {b1}; {box.count('T592 回 bob')} in bob's inbox")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["591", "592"]
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
