#!/usr/bin/env python3
"""TC-141 to TC-144 (docs/TEST.md): the App's sign-in, push, removing a
contact and deleting the account, against the local server.

    . crates/scripts/local-env.sh
    export IMBO_PUSH_LOG_ONLY=1 IMBO_WEB_ORIGIN=https://app.imbo.dev
    crates/scripts/local-env.sh up alice:bob,alice:carol
    python3 crates/scripts/app-tests.py

Prints PASS or FAIL for every step; exits 1 when any step failed. The four
cases build on each other and must run in this order. A code can be asked
for once a minute per email, so the run waits where it has to (about 3
minutes in all).

TC-145 needs Creem: afterwards `. crates/scripts/billing-env.sh`,
`crates/scripts/local-env.sh server`, then `python3 crates/scripts/app-tests.py 145`.
"""

import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

L, IMBO, M = os.environ["L"], os.environ["IMBO"], os.environ["M"]
U = "http://127.0.0.1:18080"
WEB = "https://app.imbo.dev"
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
FAILS = 0
CODE_AT = {}


def call(method, path, token=None, body=None, cookie=None, origin=False):
    """Returns (status, parsed body, headers)."""
    data = None if body is None else json.dumps(body).encode()
    r = urllib.request.Request(U + path, data=data, method=method)
    if body is not None:
        r.add_header("content-type", "application/json")
    if token:
        r.add_header("authorization", "Bearer " + token)
    if cookie:
        r.add_header("cookie", cookie)
    if origin:
        r.add_header("origin", WEB)
    try:
        with HTTP.open(r, timeout=20) as res:
            raw, status, headers = res.read(), res.status, res.headers
    except urllib.error.HTTPError as e:
        raw, status, headers = e.read(), e.code, e.headers
    try:
        parsed = json.loads(raw) if raw else None
    except ValueError:
        parsed = raw.decode(errors="replace")
    return status, parsed, headers


def check(tc, n, cond, what, got=None):
    global FAILS
    mark = "PASS" if cond else "FAIL"
    print(f"{mark} TC-{tc} #{n} {what}" + ("" if cond else f"  (got {got!r})"), flush=True)
    if not cond:
        FAILS += 1


def problem(res, status, kind):
    s, b, _ = res
    return s == status and isinstance(b, dict) and b.get("type") == f"/errors/{kind}"


def imbo(name, *args):
    env = {**os.environ, "IMBO_HOME": f"{L}/{name}"}
    p = subprocess.run([IMBO, *args], env=env, capture_output=True, text=True, timeout=60)
    return p.returncode, p.stdout + p.stderr


def as_session(name, session, tool, args):
    p = subprocess.run(
        ["python3", M, f"{L}/{name}", tool, json.dumps(args), "--session", f"claude-code:{session}"],
        env=os.environ, capture_output=True, text=True, timeout=60,
    )
    return p.stdout + p.stderr


def device_token(name):
    text = open(f"{L}/{name}/credentials").read()
    return re.search(r'^token = "(.*)"', text, re.M).group(1)


def account_ids():
    rows = [line.split() for line in open(f"{L}/ids.tsv") if line.strip()]
    return {r[0]: int(r[1]) for r in rows}


def log():
    return open(f"{L}/server.log", encoding="utf-8", errors="replace").read()


def ask_code(email):
    """Ask for a code (waiting out the one-a-minute limit) and read it from the log."""
    wait = 61 - (time.time() - CODE_AT.get(email, 0))
    if wait > 0:
        time.sleep(wait)
    pattern = re.compile(rf"mail-log-only code=(\d{{6}}) email={re.escape(email)}$", re.M)
    before = len(pattern.findall(log()))
    call("POST", "/v1/auth/code", body={"email": email})
    CODE_AT[email] = time.time()
    for _ in range(50):
        codes = pattern.findall(log())
        if len(codes) > before:
            return codes[-1]
        time.sleep(0.2)
    raise SystemExit(f"no code for {email}")


def app_login(email, name=None, platform="ios", code=None):
    body = {"email": email, "code": code or ask_code(email), "platform": platform}
    if name:
        body["name"] = name
    return call("POST", "/v1/app/session", body=body)


def web_login(email):
    code = ask_code(email)
    _, _, h = call("POST", "/v1/web/session", body={"email": email, "code": code}, origin=True)
    return h.get("set-cookie", "").split(";")[0]


FIELD = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')


def pushes():
    out = []
    for line in log().splitlines():
        if 'event="push_logged"' in line:
            f = {k: json.loads(v) if v.startswith('"') else v for k, v in FIELD.findall(line)}
            f["data"] = json.loads(f["data"])
            out.append(f)
    return out


def wait_pushes(since, account, seconds=6):
    """Pushes for `account` logged after the first `since` ones, waiting up to `seconds`."""
    deadline = time.time() + seconds
    while True:
        new = [p for p in pushes()[since:] if int(p["account_id"]) == account]
        if new or time.time() > deadline:
            return new
        time.sleep(0.3)


def conversation_with(token, peer_id):
    _, b, _ = call("GET", "/v1/conversations", token=token)
    for c in b["conversations"]:
        if c["kind"] == "direct" and c.get("peer", {}).get("id") == peer_id:
            return c
    return None


def messages_of(token, conversation):
    _, b, _ = call("GET", f"/v1/conversations/{conversation}/messages", token=token)
    return [m["body"] for m in b["messages"]]


ID = account_ids()
A, B, C = ID["alice"], ID["bob"], ID["carol"]
DA, DB, DC = device_token("alice"), device_token("bob"), device_token("carol")


def tc141():
    global TA
    r = app_login("alice@imbo.test")
    s, b, _ = r
    TA = b.get("token", "") if isinstance(b, dict) else ""
    check(141, 1, s == 200 and b["new_account"] is False and TA.startswith("imboapp_")
          and b["account"]["id"] == A, "alice signs in to the App, token starts with imboapp_", r[:2])
    code = ask_code("t141@imbo.test")
    r = app_login("t141@imbo.test", code=code)
    check(141, 2, problem(r, 422, "name-required"), "new email without a name: 422 name-required", r[:2])
    r = app_login("t141@imbo.test", name="t141", code=code)
    t141 = r[1].get("token") if isinstance(r[1], dict) else None
    check(141, 3, r[0] == 200 and r[1]["new_account"] is True and r[1]["account"]["name"] == "t141",
          "same code with a name: 200, new account t141", r[:2])
    r = call("POST", "/v1/app/session", body={"email": "alice@imbo.test", "code": "000000", "platform": "web"})
    check(141, 4, problem(r, 400, "invalid-request"), "platform web: 400 invalid-request", r[:2])
    s1, me, _ = call("GET", "/v1/me", token=TA)
    s2, conv, _ = call("GET", "/v1/conversations", token=TA)
    check(141, 5, s1 == 200 and me["account"]["email"] == "alice@imbo.test" and "device" not in me
          and s2 == 200, "App token reads /v1/me without a device, and /v1/conversations", (s1, me, s2))
    r = call("POST", "/v1/messages", token=TA,
             body={"to": "bob", "body": "T141 from the phone", "client_msg_id": "t141-1"})
    _, inbox = imbo("bob", "inbox")
    check(141, 6, r[0] == 201 and "T141 from the phone" in inbox and "alice" in inbox,
          "App sends to bob without an Origin; bob's inbox lists it", (r[:2], inbox[-300:]))
    r1 = call("PATCH", "/v1/me", token=TA, body={"name": "x141"})
    r2 = call("GET", "/v1/devices", token=TA)
    check(141, 7, problem(r1, 403, "forbidden") and problem(r2, 403, "forbidden"),
          "App can't rename or list computers: 403", (r1[:2], r2[:2]))
    r = call("DELETE", "/v1/app/session", token=t141)
    after = call("GET", "/v1/me", token=t141)
    check(141, 8, r[0] == 204 and problem(after, 401, "unauthorized"),
          "t141 signs out; the token then gets 401", (r[:2], after[:2]))
    cookie = web_login("bob@imbo.test")
    r1 = call("DELETE", "/v1/app/session", cookie=cookie, origin=True)
    r2 = call("PUT", "/v1/app/push", cookie=cookie, origin=True, body={"push_token": "ExponentPushToken[t141]"})
    r3 = call("DELETE", "/v1/app/session", token=DB)
    check(141, 9, problem(r1, 403, "forbidden") and r1[1]["detail"] == "Only the Imbo App can do this."
          and problem(r2, 403, "forbidden") and problem(r3, 403, "forbidden"),
          "web sign-in and computer token can't use the App-only endpoints: 403", (r1[:2], r2[:2], r3[:2]))
    stored = subprocess.run(["grep", "-rlF", TA, f"{L}/server", f"{L}/server.log"],
                            capture_output=True, text=True).stdout
    check(141, 10, stored == "", "the App token's text is nowhere in the server's data or log", stored[:200])


def tc142():
    global TC, COOKIE_A
    r = call("PUT", "/v1/app/push", token=TA, body={"push_token": "abc"})
    check(142, 1, problem(r, 400, "invalid-request"), "push token not ExponentPushToken[…]: 400", r[:2])
    r1 = call("PUT", "/v1/app/push", token=TA, body={"push_token": "ExponentPushToken[t142-a]"})
    r2 = call("PUT", "/v1/app/push", token=TA, body={"push_token": "ExponentPushToken[t142-a]"})
    check(142, 2, r1[0] == 204 and r2[0] == 204, "register the push token twice: 204 both times", (r1[:2], r2[:2]))
    n = len(pushes())
    imbo("bob", "send", "alice", "T142 lunch at noon?")
    got = wait_pushes(n, A)
    conv = conversation_with(TA, B)
    check(142, 3, len(got) == 1 and got[0]["title"] == "bob" and got[0]["body"] == "T142 lunch at noon?"
          and got[0]["data"] == {"conversation_id": conv["id"]},
          "bob writes alice: one push titled bob, body and conversation_id", got)
    n = len(pushes())
    imbo("alice", "send", str(A), "T142 note to self")
    call("POST", "/v1/messages", token=TA, body={"to": str(A), "body": "T142 from the App to me", "client_msg_id": "t142-self"})
    COOKIE_A = web_login("alice@imbo.test")
    w = call("POST", "/v1/messages", cookie=COOKIE_A, origin=True,
             body={"to": str(A), "body": "T142 from the web to me", "client_msg_id": "t142-web"})
    got = wait_pushes(n, A, 4)
    check(142, 4, w[0] == 201 and got == [],
          "alice writing her own inbox from a terminal, the App or the web: no push", (w[:2], got))
    imbo("alice", "dev", "fake-session", "55555555-aaaa-bbbb-cccc-000000142001", "--title", "t142")
    n = len(pushes())
    as_session("alice", "55555555-aaaa-bbbb-cccc-000000142001", "send", {"to": str(A), "body": "T142 412 tests pass"})
    got = wait_pushes(n, A)
    _, agents, _ = call("GET", "/v1/agents", token=TA)
    sid = next((a["id"] for a in agents["agents"] if a["title"] == "t142"), None)
    check(142, 5, len(got) == 1 and got[0]["title"].startswith("t142 · ") and got[0]["data"].get("agent_id") == sid,
          "her session t142 replies: push titled 't142 · <computer>' with the session's agent_id", (got, sid))
    n = len(pushes())
    r = call("POST", f"/v1/agents/{sid}/messages", token=TA, body={"body": "T142 to my session", "client_msg_id": "t142-ses"})
    imbo("bob", "send", "alice", "x" * 150)
    got = wait_pushes(n, A)
    check(142, 6, r[0] == 201 and len(got) == 1 and got[0]["body"] == "x" * 100,
          "App writes her own session: 201, no push; 150 characters from bob: one push cut to 100", (r[:2], got))
    r = app_login("carol@imbo.test")
    TC = r[1].get("token") if isinstance(r[1], dict) else None
    r = call("PUT", "/v1/app/push", token=TC, body={"push_token": "ExponentPushToken[t142-a]"})
    n = len(pushes())
    imbo("bob", "send", "alice", "T142 after carol took the phone")
    got = wait_pushes(n, A, 4)
    check(142, 7, r[0] == 204 and got == [],
          "carol signs in on the same phone and registers its token: alice gets no push", (r[:2], got))
    call("PUT", "/v1/app/push", token=TA, body={"push_token": "ExponentPushToken[t142-a]"})
    r = call("DELETE", "/v1/app/push", token=TA)
    n = len(pushes())
    imbo("bob", "send", "alice", "T142 after notifications off")
    got = wait_pushes(n, A, 4)
    check(142, 8, r[0] == 204 and got == [], "alice turns notifications off: 204, then no push", (r[:2], got))
    call("PUT", "/v1/app/push", token=TA, body={"push_token": "ExponentPushToken[t142-a]"})
    n = len(pushes())
    imbo("bob", "send", "alice", "T142 back on")
    got = wait_pushes(n, A)
    check(142, 9, len(got) == 1, "registered again: pushes again", got)


def tc143():
    imbo("alice", "send", "carol", "T143 only alice wrote here")
    r = call("DELETE", f"/v1/contacts/{C}", token=TA)
    _, contacts, _ = call("GET", "/v1/contacts", token=DC)
    code, out = imbo("carol", "send", "alice", "T143 still there?")
    check(143, 1, r[0] == 204 and all(c["id"] != A for c in contacts["contacts"]) and code != 0,
          "alice removes carol in the App: 204; carol's contacts lack alice and her send fails", (r[:2], code, out[-200:]))
    r = call("DELETE", f"/v1/contacts/{C}", token=TA)
    check(143, 2, problem(r, 403, "not-contact"), "removing carol again: 403 not-contact", r[:2])


def group_id(name, owner):
    _, out = imbo(owner, "group", "list")
    for line in out.splitlines():
        if name in line:
            return int(re.search(r"#(\d+)", line).group(1))
    raise SystemExit(f"no group {name} in {owner}'s list: {out}")


def tc144():
    # Before: alice owns g144a with bob; bob owns g144b with alice; both
    # wrote in their direct conversation and in g144b; bob also wrote to a
    # session of alice's.
    imbo("alice", "dev", "fake-session", "55555555-aaaa-bbbb-cccc-000000144001", "--title", "a144")
    imbo("alice", "group", "create", "g144a")
    ga = group_id("g144a", "alice")
    imbo("alice", "group", "invite", f"#{ga}", str(B))
    imbo("bob", "group", "join", f"#{ga}")
    imbo("bob", "group", "create", "g144b")
    gb = group_id("g144b", "bob")
    imbo("bob", "group", "invite", f"#{gb}", str(A))
    imbo("alice", "group", "join", f"#{gb}")
    imbo("bob", "send", "alice", "T144 from bob")
    imbo("bob", "send", "alice/a144", "T144 bob to alice's session")
    imbo("alice", "send", "bob", "T144 from alice")
    imbo("alice", "send", f"#{gb}", "T144 alice in g144b")
    cookie = COOKIE_A

    r = call("DELETE", "/v1/account", token=TA, body={"confirm": "delete"})
    still = call("GET", "/v1/me", token=TA)
    check(144, 1, problem(r, 400, "invalid-request") and still[0] == 200,
          "confirm 'delete' in lower case: 400, nothing deleted", (r[:2], still[0]))
    r = call("DELETE", "/v1/account", token=DA, body={"confirm": "DELETE"})
    check(144, 2, problem(r, 403, "forbidden"), "with alice's computer token: 403", r[:2])
    r = call("DELETE", "/v1/account", token=TA, body={"confirm": "DELETE"})
    check(144, 3, r[0] == 204, "with the App token and DELETE: 204", r[:2])
    r1 = call("GET", "/v1/me", token=TA)
    r2 = call("GET", "/v1/me", cookie=cookie)
    r3 = call("GET", "/v1/me", token=DA)
    code, out = imbo("alice", "inbox")
    check(144, 4, problem(r1, 401, "unauthorized") and problem(r2, 401, "unauthorized")
          and problem(r3, 401, "unauthorized") and code != 0 and "imbo login" in out,
          "App, web and computer sign-ins all get 401; alice's imbo says to sign in", (r1[:2], r2[:2], r3[:2], out[-200:]))
    conv = conversation_with(DB, A)
    bodies = messages_of(DB, conv["id"]) if conv else []
    check(144, 5, conv is not None and conv["peer"]["name"] == "Deleted account"
          and "T144 from bob" in bodies and "T144 bob to alice's session" in bodies
          and "T144 from alice" not in bodies and "T141 from the phone" not in bodies,
          "bob's conversation shows Deleted account with only bob's messages", (conv, bodies))
    notice = f"Group g144a (#{ga}) was dissolved because its owner deleted their account."
    _, inbox = imbo("bob", "inbox")
    _, groups, _ = call("GET", "/v1/groups", token=DB)
    gview = {g["id"]: g for g in groups["groups"]}
    gconv = next((c["id"] for c in call("GET", "/v1/conversations", token=DB)[1]["conversations"]
                  if c.get("group", {}).get("id") == gb), None)
    check(144, 6, notice in bodies and notice in inbox and ga not in gview
          and [m["id"] for m in gview[gb]["members"]] == [B]
          and "T144 alice in g144b" not in messages_of(DB, gconv),
          "bob gets the dissolve notice; g144a is gone; g144b has only bob, alice's line removed",
          (inbox[-300:], gview, gconv))
    _, cc, _ = call("GET", "/v1/conversations", token=DC)
    check(144, 7, all(c.get("peer", {}).get("id") != A for c in cc["conversations"]),
          "carol's conversation with alice, where only alice wrote, is gone", cc)
    _, bc, _ = call("GET", "/v1/contacts", token=DB)
    r = call("POST", "/v1/contacts/requests", token=DB, body={"account_id": A})
    check(144, 8, all(c["id"] != A for c in bc["contacts"]) and problem(r, 404, "account-not-found"),
          "alice is gone from bob's contacts; asking to add her old ID: 404 account-not-found", (bc, r[:2]))
    r = app_login("alice@imbo.test", name="alice2")
    check(144, 9, r[0] == 200 and r[1]["new_account"] is True and r[1]["account"]["id"] != A,
          "the same email signs up again as a new account with a new ID", r[:2])
    ev = [line for line in log().splitlines() if 'event="account_deleted"' in line]
    check(144, 10, len(ev) == 1 and f"account_id={A}" in ev[0] and "groups_dissolved=1" in ev[0],
          "one account_deleted event with groups_dissolved=1", ev)


def tc145():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["python3", f"{here}/creem-event.py", "subscription.paid", "--sub", "sub_t145",
         "--account", str(B), "--period-end", "+30d"],
        env=os.environ, capture_output=True, text=True,
    ).stdout
    check(145, 1, "\n200" in out, "a paid subscription sub_t145 recorded for bob", out[-200:])
    r = app_login("bob@imbo.test")
    tb = r[1].get("token") if isinstance(r[1], dict) else None
    before = log().count("creem refused")
    r = call("DELETE", "/v1/account", token=tb, body={"confirm": "DELETE"})
    check(145, 2, problem(r, 502, "payment-provider-failed"),
          "bob deletes his account, Creem can't cancel sub_t145: 502 payment-provider-failed", r[:2])
    me = call("GET", "/v1/me", token=tb)
    code, _ = imbo("bob", "inbox")
    refused = [line for line in log().splitlines() if "creem refused" in line][before:]
    check(145, 3, me[0] == 200 and code == 0 and len(refused) == 1 and "cancel" in refused[0],
          "nothing deleted: App and computer still signed in; the log has one refused cancel",
          (me[:2], code, refused))


if __name__ == "__main__":
    TA = TC = COOKIE_A = None
    if sys.argv[1:] == ["145"]:
        tc145()
        print(f"{FAILS} failed")
        sys.exit(1 if FAILS else 0)
    for case in (tc141, tc142, tc143, tc144):
        try:
            case()
        except Exception as e:  # report and go on with the next case
            check(case.__name__[2:], "-", False, "crashed", repr(e))
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
