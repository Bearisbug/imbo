#!/usr/bin/env python3
"""TC-338 to TC-341 (docs/TEST.md): changing the sign-in email (API-075,
API-076), against the local server, with real HTTP requests.

    . crates/scripts/local-env.sh
    python3 crates/scripts/email-tests.py <RUN-ID> [338 339 340 341]

Each case starts its own server and seed with `local-env.sh down` and
`local-env.sh up alice:bob`, under the variables its precondition names, and
the last one is taken down at the end. Prints PASS or FAIL for every step and
writes each case's lines to docs/test-runs/<run>-tc-NNN.txt (with
RUN_HEADER, if set, on the first line); exits 1 when any step failed. About 6
minutes for all four, most of it waiting out the one-code-a-minute limit.
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
LOG = f"{L}/server.log"
ENV = os.path.join(os.path.dirname(os.path.abspath(__file__)), "local-env.sh")
ORIGIN = "https://app.imbo.dev"
OUT = []
FAILS = 0


def call(method, path, body=None, bearer=None, cookie=None, xff=None):
    h = {"content-type": "application/json"}
    if bearer:
        h["authorization"] = f"Bearer {bearer}"
    if cookie:
        h["cookie"] = cookie
        h["origin"] = ORIGIN
    if xff:
        h["x-forwarded-for"] = xff
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request(method, path, body=json.dumps(body) if body is not None else None, headers=h)
    r = c.getresponse()
    raw = r.read()
    set_cookie = r.getheader("set-cookie") or ""
    c.close()
    return r.status, (json.loads(raw) if raw else None), set_cookie


def lines(pattern):
    return [l for l in open(LOG, errors="replace") if re.search(pattern, l)]


def code_of(email):
    for _ in range(50):
        found = lines(rf"mail-log-only code=\d{{6}} email={re.escape(email)}$")
        if found:
            return re.search(r"code=(\d{6})", found[-1]).group(1)
        time.sleep(0.2)
    raise SystemExit(f"no code for {email}")


def ask_code(email, xff=None):
    s, b, _ = call("POST", "/v1/auth/code", {"email": email}, xff=xff)
    return s, b


def app_token(email):
    s, b = ask_code(email)
    assert s == 202, (s, b)
    s, b, _ = call("POST", "/v1/app/session", {"email": email, "code": code_of(email), "platform": "ios"})
    assert s == 200, (s, b)
    return b["token"]


def web_cookie(email):
    s, b = ask_code(email)
    assert s == 202, (s, b)
    s, b, ck = call("POST", "/v1/web/session", {"email": email, "code": code_of(email)}, cookie="x=1")
    assert s == 200, (s, b)
    return ck.split(";")[0]


def computer_token(name):
    return re.search(r'^token = "(.*)"', open(f"{L}/{name}/credentials").read(), re.M).group(1)


def computer_login(email, code, tc, name=None):
    body = {"email": email, "code": code, "device_name": f"t{tc}", "platform": "macos", "client_version": "0.2.14"}
    if name:
        body["name"] = name
    s, b, _ = call("POST", "/v1/auth/verify", body)
    return s, b


def check(tc, n, what, ok, detail):
    global FAILS
    FAILS += not ok
    line = f"{'PASS' if ok else 'FAIL'} TC-{tc} #{n} {what}: {detail}"
    OUT.append(line)
    print(line, flush=True)


def typ(b):
    return (b or {}).get("type")


def tc338():
    a = app_token("alice@imbo.test")
    me = call("GET", "/v1/me", bearer=a)[1]["account"]["id"]
    time.sleep(61)
    ck = web_cookie("alice@imbo.test")
    mark = len(open(LOG, errors="replace").readlines())
    s, b, _ = call("POST", "/v1/me/email/code", {"email": "New338@imbo.test"}, bearer=a)
    k = code_of("new338@imbo.test")
    check(338, 1, "code to the new email", s == 202 and b == {"retry_after_s": 60}, f"{s} {b}")
    s, b, _ = call("POST", "/v1/me/email", {"email": "new338@imbo.test", "code": k}, bearer=a)
    check(338, 2, "changed", s == 200 and b["account"]["id"] == me and b["account"]["email"] == "new338@imbo.test", f"{s} {b}")
    s, b, _ = call("POST", "/v1/me/email", {"email": "new338@imbo.test", "code": k}, bearer=a)
    check(338, 3, "same request again", s == 400 and typ(b) == "/errors/code-invalid", f"{s} {b}")
    got = [call("GET", "/v1/me", **kw)[:2] for kw in ({"bearer": a}, {"cookie": ck}, {"bearer": computer_token("alice")})]
    check(338, 4, "every sign-in stays", all(s == 200 and b["account"]["email"] == "new338@imbo.test" for s, b in got),
          [(s, b["account"]["email"] if b else b) for s, b in got])
    new = open(LOG, errors="replace").readlines()[mark:]
    old = [l for l in new if "email=alice@imbo.test" in l or "to=alice@imbo.test" in l]
    check(338, 5, "nothing to the old email", not old, old)
    time.sleep(61)
    ask_code("new338@imbo.test")
    s, b = computer_login("new338@imbo.test", code_of("new338@imbo.test"), 338)
    check(338, 6, "new email signs in to the same account", s == 200 and b["account"]["id"] == me and b["new_account"] is False, f"{s} {b and b.get('account')}")
    ask_code("alice@imbo.test")
    c = code_of("alice@imbo.test")
    s, b = computer_login("alice@imbo.test", c, 338)
    check(338, 7, "old email belongs to no one", s == 422 and typ(b) == "/errors/name-required", f"{s} {b}")
    s, b = computer_login("alice@imbo.test", c, 338, "alice338")
    check(338, 8, "old email signs up anew", s == 200 and b["new_account"] is True and b["account"]["id"] != me, f"{s} {b and b.get('account')}")


def tc339():
    a = app_token("alice@imbo.test")
    for n, email, want, detail in [
        (1, "Bob@imbo.test", (409, "/errors/email-taken"), "That email is already used by another Imbo account."),
        (2, "not-an-email", (400, "/errors/invalid-email"), None),
        (3, "ALICE@imbo.test", (400, "/errors/invalid-email"), "That's already your email."),
        (4, "Review@imbo.test", (400, "/errors/invalid-email"), None),
        (5, "t339@example.com", (403, "/errors/signup-closed"), "This server doesn't accept emails from that domain."),
    ]:
        s, b, _ = call("POST", "/v1/me/email/code", {"email": email}, bearer=a)
        ok = (s, typ(b)) == want and (detail is None or b.get("detail") == detail)
        if n in (1, 4, 5):
            ok = ok and not lines(rf"email={re.escape(email.lower())}$")
        check(339, n, email, ok, f"{s} {b}")


def tc340():
    a = app_token("alice@imbo.test")
    sub = lambda email, code: call("POST", "/v1/me/email", {"email": email, "code": code}, bearer=a)[:2]
    s, b, _ = call("POST", "/v1/me/email/code", {"email": "t340a@imbo.test"}, bearer=a)
    k1 = code_of("t340a@imbo.test")
    check(340, 1, "code", s == 202, f"{s} {b}")
    wrong = "000000" if k1 != "000000" else "111111"
    s, b = sub("t340a@imbo.test", wrong)
    check(340, 2, "wrong code", s == 400 and typ(b) == "/errors/code-invalid" and b["detail"] == "Wrong code. 4 tries left.", f"{s} {b}")
    subprocess.run([os.environ["S"], "dev", "expire-code", "t340a@imbo.test"],
                   env={**os.environ, "IMBO_DATA_DIR": f"{L}/server"}, stdout=subprocess.DEVNULL, check=True)
    s, b = sub("t340a@imbo.test", k1)
    check(340, 3, "expired", s == 410 and typ(b) == "/errors/code-expired", f"{s} {b}")
    time.sleep(61)
    s0, _, _ = call("POST", "/v1/me/email/code", {"email": "t340a@imbo.test"}, bearer=a)
    k2 = code_of("t340a@imbo.test")
    bad = "000000" if k2 != "000000" else "111111"
    got = [sub("t340a@imbo.test", bad) for _ in range(5)]
    last = sub("t340a@imbo.test", k2)
    email = call("GET", "/v1/me", bearer=a)[1]["account"]["email"]
    ok = (s0 == 202 and all(x[0] == 400 and typ(x[1]) == "/errors/code-invalid" for x in got[:4])
          and got[4][0] == 429 and typ(got[4][1]) == "/errors/code-locked" and last[0] in (400, 429) and email == "alice@imbo.test")
    check(340, 4, "locked", ok, f"{s0} {[x[0] for x in got]} {last} {email}")
    s, b = ask_code("t340b@imbo.test")
    l_code = code_of("t340b@imbo.test")
    s2, b2 = sub("t340b@imbo.test", l_code)
    check(340, 5, "a sign-in code can't change the email", s == 202 and s2 == 400 and typ(b2) == "/errors/code-invalid", f"{s} {s2} {b2}")
    s, _, _ = call("POST", "/v1/me/email/code", {"email": "t340c@imbo.test"}, bearer=a)
    c = code_of("t340c@imbo.test")
    s2, b2 = computer_login("t340c@imbo.test", c, 340, "t340c")
    check(340, 6, "a change code can't sign in", s == 202 and s2 == 400 and typ(b2) == "/errors/code-invalid", f"{s} {s2} {b2}")
    time.sleep(61)
    s, _ = ask_code("t340c@imbo.test")
    s2, b2 = sub("t340c@imbo.test", c)
    check(340, 7, "a new sign-in code leaves the change code", s == 202 and s2 == 200 and b2["account"]["email"] == "t340c@imbo.test", f"{s} {s2} {b2}")
    s, _, _ = call("POST", "/v1/me/email/code", {"email": "t340b@imbo.test"}, bearer=a)
    s2, b2 = computer_login("t340b@imbo.test", l_code, 340, "t340b")
    check(340, 8, "a new change code leaves the sign-in code", s == 202 and s2 == 200 and b2["new_account"] is True, f"{s} {s2} {b2 and b2.get('account')}")


def tc341():
    a = app_token("alice@imbo.test")
    pc = computer_token("alice")
    s1, b1, _ = call("POST", "/v1/me/email/code", {"email": "t341a@imbo.test"}, bearer=pc)
    s2, b2, _ = call("POST", "/v1/me/email", {"email": "t341a@imbo.test", "code": "123456"}, bearer=pc)
    check(341, 1, "a computer can't", s1 == s2 == 403 and typ(b1) == typ(b2) == "/errors/forbidden" and not lines("email=t341a@imbo.test"), f"{s1} {b1} {s2}")
    t0 = time.time()
    s, _, _ = call("POST", "/v1/me/email/code", {"email": "t341b@imbo.test"}, bearer=a)
    k = code_of("t341b@imbo.test")
    s2, b2, _ = call("POST", "/v1/me/email/code", {"email": "t341b@imbo.test"}, bearer=a)
    check(341, 2, "one a minute", s == 202 and s2 == 429 and typ(b2) == "/errors/rate-limited", f"{s} {s2} {b2}")
    got = [ask_code(f"t341-{i}@imbo.test", xff="10.3.41.1")[0] for i in range(1, 21)]
    s, b, _ = call("POST", "/v1/me/email/code", {"email": "t341c@imbo.test"}, bearer=a, xff="10.3.41.1")
    check(341, 3, "20 an hour from a network", got == [202] * 20 and s == 429 and typ(b) == "/errors/rate-limited", f"{got} {s} {b}")
    time.sleep(max(0, 61 - (time.time() - t0)))
    ask_code("t341b@imbo.test")
    s, b = computer_login("t341b@imbo.test", code_of("t341b@imbo.test"), 341, "t341b")
    check(341, 4, "someone signs up with it", s == 200 and b["new_account"] is True, f"{s} {b and b.get('account')}")
    s, b, _ = call("POST", "/v1/me/email", {"email": "t341b@imbo.test", "code": k}, bearer=a)
    check(341, 5, "taken by then", s == 409 and typ(b) == "/errors/email-taken", f"{s} {b}")
    s, b, _ = call("POST", "/v1/me/email", {"email": "t341b@imbo.test", "code": k}, bearer=a)
    check(341, 6, "the code is spent", s == 400 and typ(b) == "/errors/code-invalid", f"{s} {b}")
    email = call("GET", "/v1/me", bearer=a)[1]["account"]["email"]
    check(341, 7, "still alice@", email == "alice@imbo.test", email)


CASES = {"338": tc338, "339": tc339, "340": tc340, "341": tc341}
# What each case's precondition adds before `up`.
EXTRA = {"339": {"IMBO_REVIEW_EMAIL": "review@imbo.test", "IMBO_REVIEW_CODE": "246810", "IMBO_SIGNUP_EMAIL_DOMAINS": "imbo.test"}}

if __name__ == "__main__":
    run = sys.argv[1]
    header = os.environ.get("RUN_HEADER", "")
    for tc in sys.argv[2:] or list(CASES):
        env = {**os.environ, "IMBO_WEB_ORIGIN": ORIGIN, **EXTRA.get(tc, {})}
        subprocess.run([ENV, "down"], env=env, capture_output=True)
        subprocess.run([ENV, "up", "alice:bob"], env=env, capture_output=True, check=True)
        try:
            CASES[tc]()
        except Exception as e:
            check(tc, "-", False, "crashed", repr(e))
        path = os.path.join(os.environ.get("EVIDENCE_DIR", "docs/test-runs"), f"{run.lower()}-tc-{tc}.txt")
        with open(path, "w") as f:
            f.write(f"{run} TC-{tc} · {header}\n" + "\n".join(OUT) + "\n")
        OUT.clear()
    subprocess.run([ENV, "down"], capture_output=True)
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
