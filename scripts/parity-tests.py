#!/usr/bin/env python3
"""TC-368 to TC-370 (docs/TEST.md): what the web app and the App can do the
terminal's way (API-005..008, API-022, API-037..039), contact requests by
email (API-008) and renaming groups (API-077), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/parity-tests.py <RUN-ID> [368 369 370]

Each case starts its own server and seed with `local-env.sh down` and
`local-env.sh up …`, under the variables its precondition names, and the last
one is taken down at the end. Prints PASS or FAIL for every step and writes
each case's lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed. About a minute each.
Verification codes, tokens and cookies are never written out.
"""

import http.client
import json
import os
import re
import subprocess
import sys
import time

L = os.environ["L"]
IMBO = os.environ["IMBO"]
LOG = f"{L}/server.log"
HERE = os.path.dirname(os.path.abspath(__file__))
ENV = os.path.join(HERE, "local-env.sh")
ORIGIN = "https://app.imbo.dev"
OUT = []
FAILS = 0
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\(B")


def call(method, path, body=None, bearer=None, cookie=None, origin=True):
    h = {"content-type": "application/json"}
    if bearer:
        h["authorization"] = f"Bearer {bearer}"
    if cookie:
        h["cookie"] = cookie
        if origin:
            h["origin"] = ORIGIN
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request(method, path, body=json.dumps(body) if body is not None else None, headers=h)
    r = c.getresponse()
    raw = r.read()
    set_cookie = r.getheader("set-cookie") or ""
    c.close()
    return r.status, (json.loads(raw) if raw else None), set_cookie


def code_of(email, before):
    pat = re.compile(rf"mail-log-only code=(\d{{6}}) email={re.escape(email)}$")
    for _ in range(50):
        found = [m.group(1) for m in map(pat.search, open(LOG, errors="replace")) if m]
        if len(found) > before:
            return found[-1]
        time.sleep(0.2)
    raise SystemExit(f"no code for {email}")


def sign_in(email, path, extra, web):
    pat = re.compile(rf"mail-log-only code=\d{{6}} email={re.escape(email)}$")
    before = sum(1 for l in open(LOG, errors="replace") if pat.search(l))
    s, b, _ = call("POST", "/v1/auth/code", {"email": email})
    assert s == 202, (s, b)
    return call("POST", path, {"email": email, "code": code_of(email, before), **extra},
                cookie="x=1" if web else None)


def app_token(email):
    s, b, _ = sign_in(email, "/v1/app/session", {"platform": "ios"}, False)
    assert s == 200, (s, b)
    return b["token"]


def web_cookie(email):
    s, b, ck = sign_in(email, "/v1/web/session", {}, True)
    assert s == 200, (s, b)
    return ck.split(";")[0]


def computer_token(home):
    return re.search(r'^token = "(.*)"', open(f"{home}/credentials").read(), re.M).group(1)


def ids():
    return {l.split("\t")[0]: int(l.split("\t")[1]) for l in open(f"{L}/ids.tsv") if "\t" in l}


def imbo(who, *args, env=None):
    home = who if who.startswith("/") else f"{L}/{who}"
    r = subprocess.run([IMBO, *args], capture_output=True, text=True,
                       env={**os.environ, "IMBO_HOME": home, **(env or {})})
    return r.returncode, ANSI.sub("", r.stdout + r.stderr).strip()


def up(contacts, **env):
    subprocess.run(["sh", ENV, "down"], capture_output=True)
    os.environ.pop("IMBO_ADMIN_EMAILS", None)
    os.environ.update(env)
    subprocess.run(["sh", ENV, "up", contacts], check=True, capture_output=True)


def extra(names):
    subprocess.run([os.environ["S"], "dev", "seed", "--homes", f"{L}/extra", "--names", names],
                   check=True, capture_output=True, env={**os.environ, "IMBO_DATA_DIR": f"{L}/server"})


def group_id(token, name):
    s, b, _ = call("GET", "/v1/groups", bearer=token)
    return next((g["id"] for g in b["groups"] if g["name"] == name), None)


def group_named(token, gid):
    s, b, _ = call("GET", "/v1/groups", bearer=token)
    return next((g["name"] for g in b["groups"] if g["id"] == gid), None)


def make_group(name, members):
    imbo("alice", "group", "create", name)
    for who, aid in members:
        imbo("alice", "group", "invite", name, str(aid))
        imbo(who, "group", "join", name)
    return group_id(computer_token(f"{L}/alice"), name)


def check(tc, n, what, ok, detail):
    global FAILS
    FAILS += not ok
    line = f"{'PASS' if ok else 'FAIL'} TC-{tc} #{n} {what}: {detail}"
    OUT.append(line)
    print(line, flush=True)


def typ(b):
    return (b or {}).get("type")


def tc368():
    up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=ORIGIN, IMBO_ADMIN_EMAILS="carol@imbo.test")
    i = ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    a, b = app_token("alice@imbo.test"), app_token("bob@imbo.test")
    c = web_cookie("carol@imbo.test")
    g = make_group("t368", [("bob", B), ("carol", C)])
    OUT.append(f"前置:群 t368 #{g}")

    s, r, _ = call("PATCH", "/v1/me", {"name": "Carol 368"}, cookie=c, origin=False)
    check(368, 1, "网页不带 Origin 改名", s == 403 and typ(r) == "/errors/forbidden", f"{s} {typ(r)}")
    s, r, _ = call("PATCH", "/v1/me", {"name": "Carol 368"}, cookie=c)
    acc = (r or {}).get("account", {})
    check(368, 2, "网页带 Origin 改名", s == 200 and acc.get("name") == "Carol 368" and acc.get("admin") is True,
          f"{s} name={acc.get('name')} admin={acc.get('admin')}")
    s, r, _ = call("PATCH", "/v1/me", {"name": "Alice 368"}, bearer=a)
    acc = (r or {}).get("account", {})
    _, cs, _ = call("GET", "/v1/contacts", bearer=b)
    seen = next((x["name"] for x in cs["contacts"] if x["id"] == A), None)
    check(368, 3, "App 改名", s == 200 and acc.get("name") == "Alice 368" and "admin" not in acc and seen == "Alice 368",
          f"{s} name={acc.get('name')} admin 字段={'有' if 'admin' in acc else '没有'};bob 看到 {seen}")
    s, r, _ = call("DELETE", f"/v1/groups/{g}/members/{C}", bearer=b)
    check(368, 4, "不是群主移人", s == 403 and typ(r) == "/errors/not-owner", f"{s} {typ(r)}")
    s, r, _ = call("DELETE", f"/v1/groups/{g}/members/me", bearer=b)
    still = group_id(b, "t368")
    check(368, 5, "App 退群", s == 204 and still is None, f"{s};bob 的群列表里 t368:{'有' if still else '没有'}")
    s, r, _ = call("PUT", f"/v1/groups/{g}/owner", {"account_id": C}, bearer=a)
    owner = ((r or {}).get("group") or {}).get("owner") or {}
    check(368, 6, "App 转让群主", s == 200 and owner.get("id") == C, f"{s} owner={owner.get('id')}")
    s, r, _ = call("DELETE", f"/v1/groups/{g}/members/{A}", cookie=c)
    still = group_id(a, "t368")
    want = f"You were removed from group t368 (#{g}) by Carol 368."
    time.sleep(1)
    _, inbox = imbo("alice", "inbox")
    check(368, 7, "网页移人", s == 204 and still is None and want in inbox,
          f"{s};alice 的群列表里 t368:{'有' if still else '没有'};alice imbo inbox {'有' if want in inbox else '没有'}「{want}」")
    s, r, _ = call("DELETE", f"/v1/groups/{g}", cookie=c)
    s2, r2, _ = call("GET", "/v1/groups", cookie=c)
    left = [x["name"] for x in r2["groups"]]
    check(368, 8, "网页解散", s == 204 and "t368" not in left, f"{s};carol 的群列表 {left}")
    s, r, _ = call("GET", "/v1/devices", bearer=a)
    devs = (r or {}).get("devices", [])
    check(368, 9, "App 看电脑", s == 200 and len(devs) == 1 and devs[0]["current"] is False,
          f"{s} {len(devs)} 台,current={[d['current'] for d in devs]}")
    d = devs[0]["id"] if devs else "none"
    s, r, _ = call("DELETE", f"/v1/devices/{d}", bearer=a)
    s2, _, _ = call("GET", "/v1/me", bearer=computer_token(f"{L}/alice"))
    check(368, 10, "App 吊销电脑", s == 204 and s2 == 401, f"{s};alice 的电脑 token GET /v1/me → {s2}")


def tc369():
    up("alice:bob", IMBO_WEB_ORIGIN=ORIGIN)
    extra("dave")
    i = ids()
    A = i["alice"]
    dt = computer_token(f"{L}/extra/dave")
    _, me, _ = call("GET", "/v1/me", bearer=dt)
    D = me["account"]["id"]
    a = app_token("alice@imbo.test")
    c = web_cookie("carol@imbo.test")

    s, r, _ = call("POST", "/v1/contacts/requests", {"email": " Dave@IMBO.test ", "note": "T369"}, bearer=a)
    to = ((r or {}).get("request") or {}).get("to") or {}
    _, rq, _ = call("GET", "/v1/requests", bearer=dt)
    got = any(q["from"]["id"] == A and q.get("note") == "T369" for q in rq["requests"])
    check(369, 1, "App 按邮箱申请", s == 201 and to.get("id") == D and to.get("name") == "dave" and got,
          f"{s} to={to.get('id')}/{to.get('name')};dave 的申请列表{'有' if got else '没有'} alice 的 T369")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": " Dave@IMBO.test ", "note": "T369"}, bearer=a)
    check(369, 2, "再发一次", s == 409 and typ(r) == "/errors/request-pending", f"{s} {typ(r)}")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "bob@imbo.test"}, bearer=a)
    check(369, 3, "已是联系人", s == 409 and typ(r) == "/errors/already-contacts", f"{s} {typ(r)}")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "nobody369@imbo.test"}, bearer=a)
    det = (r or {}).get("detail")
    check(369, 4, "没注册的邮箱", s == 404 and typ(r) == "/errors/account-not-found"
          and det == "No account with email nobody369@imbo.test.", f"{s} {typ(r)} {det}")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "dave"}, bearer=a)
    check(369, 5, "格式不对", s == 400 and typ(r) == "/errors/invalid-email", f"{s} {typ(r)}")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "ALICE@imbo.test"}, bearer=a)
    check(369, 6, "本人邮箱", s == 422 and typ(r) == "/errors/cannot-add-self", f"{s} {typ(r)}")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "dave@imbo.test", "account_id": D}, bearer=a)
    s2, r2, _ = call("POST", "/v1/contacts/requests", {"note": "T369x"}, bearer=a)
    check(369, 7, "两个都带、都不带", s == s2 == 400 and typ(r) == typ(r2) == "/errors/invalid-request",
          f"{s} {typ(r)};{s2} {typ(r2)}")
    rc, out = imbo("carol", "contact", "add", "Alice@imbo.test", "--note", "T369c")
    check(369, 8, "终端按邮箱加", rc == 0 and f"Request sent to alice ({A})" in out, f"退出码 {rc};{out!r}")
    rc, out = imbo("alice", "contact", "add", "carol@imbo.test")
    _, lst = imbo("alice", "contact", "list")
    check(369, 9, "对方先申请过", rc == 0 and "carol had already asked to add you. You're contacts now." in out
          and "carol" in lst, f"退出码 {rc};{out!r};contact list {'有' if 'carol' in lst else '没有'} carol")
    s, r, _ = call("POST", "/v1/contacts/requests", {"email": "dave@imbo.test"}, cookie=c)
    to = ((r or {}).get("request") or {}).get("to") or {}
    check(369, 10, "网页按邮箱申请", s == 201 and to.get("id") == D, f"{s} to={to.get('id')}")


def tc370():
    up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=ORIGIN)
    i = ids()
    A, B, C = i["alice"], i["bob"], i["carol"]
    a, b = app_token("alice@imbo.test"), app_token("bob@imbo.test")
    c = web_cookie("carol@imbo.test")
    g = make_group("t370", [("bob", B), ("carol", C)])
    sid = "55555555-aaaa-bbbb-cccc-000000370001"
    imbo("alice", "dev", "fake-session", sid, "--title", "a370")
    imbo("alice", "group", "sessions", "t370", "--sessions", "370001")
    OUT.append(f"前置:群 t370 #{g}")

    rc, out = imbo("alice", "group", "rename", "t370", "core370")
    seen = group_named(b, g)
    check(370, 1, "终端改名", rc == 0 and "Renamed t370 to core370" in out and seen == "core370",
          f"退出码 {rc};{out!r};bob 看到 {seen}")
    s, r, _ = call("PATCH", f"/v1/groups/{g}", {"name": "core370"}, bearer=a)
    grp = (r or {}).get("group") or {}
    check(370, 2, "同名", s == 200 and grp.get("id") == g and grp.get("name") == "core370",
          f"{s} {grp.get('id')} {grp.get('name')}")
    s, r, _ = call("PATCH", f"/v1/groups/{g}", {"name": "  "}, bearer=a)
    s2, r2, _ = call("PATCH", f"/v1/groups/{g}", {"name": "#core"}, bearer=a)
    check(370, 3, "不合规", s == s2 == 400 and typ(r) == typ(r2) == "/errors/invalid-request",
          f"{s} {typ(r)};{s2} {typ(r2)}")
    s, r, _ = call("PATCH", f"/v1/groups/{g}", {"name": "bob370"}, bearer=b)
    det = (r or {}).get("detail") or ""
    check(370, 4, "不是群主", s == 403 and typ(r) == "/errors/not-owner" and f"alice ({A})" in det,
          f"{s} {typ(r)} {det}")
    rc, out = imbo("alice", "group", "rename", "core370", "agent370", env={"CLAUDE_CODE_SESSION_ID": sid})
    seen = group_named(b, g)
    want = "Only the account owner can do this, from a terminal outside any agent session."
    check(370, 5, "会话里改名", rc == 1 and want in out and seen == "core370",
          f"退出码 {rc};{out!r};bob 看到 {seen}")
    imbo("alice", "group", "transfer", "core370", str(C), "--yes")
    s, r, _ = call("PATCH", f"/v1/groups/{g}", {"name": "web370"}, cookie=c)
    grp = (r or {}).get("group") or {}
    check(370, 6, "网页改名", s == 200 and grp.get("name") == "web370" and (grp.get("owner") or {}).get("id") == C,
          f"{s} {grp.get('name')} owner={(grp.get('owner') or {}).get('id')}")
    imbo("bob", "send", f"#{g}", "T370 after rename")
    time.sleep(2)
    r = subprocess.run(["python3", os.environ["M"], f"{L}/alice", "inbox", "{}", "--session", f"claude-code:{sid}"],
                       capture_output=True, text=True)
    text = r.stdout
    lines = text.splitlines()
    at = next((k for k, l in enumerate(lines) if "T370 after rename" in l), None)
    head = next((lines[k] for k in range(at, -1, -1) if "@ " in lines[k]), "") if at is not None else ""
    check(370, 7, "改名后的群消息", at is not None and "from bob · terminal @ web370" in head,
          f"{'收到' if at is not None else '没收到'};发件人一行 {head.strip()!r}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["368", "369", "370"]
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
    print(f"failed steps: {FAILS}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
