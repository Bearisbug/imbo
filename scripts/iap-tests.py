#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["psycopg[binary]>=3.2"]
# ///
"""TC-261 to TC-263, TC-265 to TC-269, TC-282, TC-285 to TC-287, TC-291 to
TC-293, TC-295, TC-296 and TC-298 (docs/TEST.md, 应用内购买), and TC-297 (App),
against the local server, with real HTTP requests, the fake App Store of
apple-event.py and real database queries.

    . crates/scripts/local-env.sh
    crates/scripts/iap-tests.py <RUN-ID> [TC numbers…]

Every case builds its own environment as its 前置 says: `local-env.sh down`,
then billing-env.sh, the case's own exports, apple-env.sh, `local-env.sh up
<contacts>` and App tokens, so the cases run in any order. Prints PASS or
FAIL for every step and writes each case's lines to
docs/test-runs/<run>-tc-NNN.txt (EVIDENCE_DIR overrides the directory;
RUN_HEADER goes into the first line); exits 1 when any step failed. Leaves
the last case's environment up; `local-env.sh down` ends it.

Tokens, cookies and codes are never written out.
"""

import datetime as dt
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request

import psycopg

HERE = os.path.dirname(os.path.abspath(__file__))
L = os.environ["L"]
LOCAL_ENV = os.path.join(HERE, "local-env.sh")
AE = os.path.join(HERE, "apple-event.py")
U = "http://127.0.0.1:18080"
WEB = "https://app.imbo.dev"
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
OLD = os.path.expanduser("~/.cache/imbo-wt/bin/imbo-server-pre0009")
BASE = dict(os.environ)
ENV = dict(BASE)
FAILS = 0
CODE_AT = {}
OUT = []
SECRETS = []
ID = {}
APP = {}


# ---------- plumbing ----------

def hide(text):
    text = str(text)
    for s in SECRETS:
        if s:
            text = text.replace(s, "<secret>")
    return text


def say(line):
    line = hide(line)
    print(line, flush=True)
    OUT.append(line)


def check(tc, n, cond, what, got=None):
    global FAILS
    say(f"{'PASS' if cond else 'FAIL'} TC-{tc} #{n} {what}")
    if got is not None:
        seen = repr(got)
        say("     got " + (seen if not cond or len(seen) <= 700 else seen[:700] + " …"))
    if not cond:
        FAILS += 1
    return cond


def call(method, path, token=None, body=None, cookie=None, origin=None, base=U):
    """(status, parsed body)."""
    data = None if body is None else json.dumps(body).encode()
    r = urllib.request.Request(base + path, data=data, method=method)
    if body is not None:
        r.add_header("content-type", "application/json")
    if token:
        r.add_header("authorization", "Bearer " + token)
    if cookie:
        r.add_header("cookie", cookie)
    if origin:
        r.add_header("origin", origin)
    try:
        with HTTP.open(r, timeout=60) as res:
            raw, status = res.read(), res.status
    except urllib.error.HTTPError as e:
        raw, status = e.read(), e.code
    try:
        return status, (json.loads(raw) if raw else None)
    except ValueError:
        return status, raw.decode(errors="replace")


def problem(res, status, kind):
    s, b = res
    return s == status and isinstance(b, dict) and b.get("type") == f"/errors/{kind}"


def brief(res):
    s, b = res
    if isinstance(b, dict) and "type" in b:
        return (s, b["type"], b.get("detail"))
    return (s, b)


def run(args, env=None, timeout=120):
    p = subprocess.run(args, env={**ENV, **(env or {})}, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout + p.stderr


def ae(*args):
    """apple-event.py; for --submit and notify, (status, body) from its output."""
    code, out = run([AE, *args])
    lines = out.strip().splitlines()
    if args[0] == "transaction" and "--submit" in args or args[0] == "notify":
        status = int(lines[0]) if lines and lines[0].isdigit() else None
        body = "\n".join(lines[1:])
        try:
            body = json.loads(body) if body else None
        except ValueError:
            pass
        return status, body
    return code, out


def tx(account, original, *more, submit=None, product="monthly", env="Sandbox", expires="+30d"):
    args = ["transaction", "--account", str(account), "--original", str(original), "--product", product,
            "--env", env, "--expires", expires, *more]
    if submit:
        args += ["--submit", submit]
    return ae(*args)


def status(original, *more):
    return ae("status", str(original), *more)


def notify(kind, original, *more, env="Sandbox"):
    return ae("notify", kind, "--original", str(original), "--env", env, *more)


def imbo(name, *args):
    e = {"IMBO_HOME": f"{L}/{name}"}
    return run([ENV["IMBO"], *args], env=e, timeout=90)


def log():
    return open(f"{L}/server.log").read()


def outcome():
    lines = [ln for ln in log().splitlines() if 'event="billing_event"' in ln]
    m = re.search(r'outcome="?([a-z]+)', lines[-1]) if lines else None
    return m and m.group(1)


def fake():
    try:
        return open(f"{L}/apple/fake.log").read().splitlines()
    except FileNotFoundError:
        return []


def pgurl(data_dir=None):
    pid = f"{data_dir or L + '/server'}/pg/pgdata/postmaster.pid"
    port = open(pid).read().splitlines()[3].strip()
    return f"postgres://postgres@127.0.0.1:{port}/postgres"


def sql(q, data_dir=None):
    with psycopg.connect(pgurl(data_dir), autocommit=True) as c:
        return c.execute(q).fetchall()


def near(t, delta, slack=dt.timedelta(minutes=3)):
    """An RFC 3339 time within slack of now + delta."""
    if isinstance(t, str):
        t = dt.datetime.fromisoformat(t.replace("Z", "+00:00"))
    return abs(t - (dt.datetime.now(dt.timezone.utc) + delta)) < slack


DAY = dt.timedelta(days=1)


def billing(name):
    return call("GET", "/v1/billing", token=APP[name])


def sub_of(name):
    return (billing(name)[1] or {}).get("subscription")


def wait_health(base=U, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        try:
            s, b = call("GET", "/v1/health", base=base)
            if s == 200:
                return b
        except (urllib.error.URLError, ConnectionError, OSError):
            pass
        time.sleep(0.3)
    raise RuntimeError(f"no health at {base}")


def server(extra=None, drop=()):
    """`local-env.sh server` with this env, plus `extra`, without `drop`."""
    global ENV
    env = {k: v for k, v in ENV.items() if k not in drop}
    env.update(extra or {})
    subprocess.run([LOCAL_ENV, "server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   timeout=120, check=True)


def code_gap(email):
    """The server sends one code per email per minute."""
    wait = CODE_AT.get(email, 0) + 62 - time.time()
    if wait > 0:
        time.sleep(wait)
    CODE_AT[email] = time.time()


def app_login(name):
    code_gap(f"{name}@imbo.test")
    code, out = run([os.path.join(HERE, "app-login.sh"), f"{name}@imbo.test"])
    token = json.loads(out.splitlines()[0])["token"]
    SECRETS.append(token)
    with open(f"{L}/{name}.app", "w") as f:
        f.write(token + "\n")
    APP[name] = token


def web_login(email, jar, name=None):
    code_gap(email)
    run([os.path.join(HERE, "web-login.sh"), email, jar, *([name] if name else [])])
    cookie = None
    for line in open(jar):
        parts = line.rstrip("\n").split("\t")
        if len(parts) == 7:
            cookie = f"{parts[5]}={parts[6]}"
    SECRETS.append(cookie)
    return cookie


def device_token(name):
    t = re.search(r'^token = "(.*)"', open(f"{L}/{name}/credentials").read(), re.M).group(1)
    SECRETS.append(t)
    return t


def setup(contacts, exports="", tokens=("alice", "bob", "carol")):
    """The case's 前置: down, billing-env.sh, `exports`, apple-env.sh, up, App tokens."""
    global ENV
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    envfile = f"{L}.iap-env"
    script = f"""set -e
. "{LOCAL_ENV}"
. "{HERE}/billing-env.sh"
{exports}
. "{HERE}/apple-env.sh"
"{LOCAL_ENV}" up {contacts}
env -0 > "{envfile}"
"""
    with open(f"{L}.iap-setup.log", "w") as out:
        subprocess.run(["bash", "-c", script], env=BASE, stdout=out, stderr=out, timeout=300, check=True)
    raw = open(envfile, "rb").read().decode()
    os.remove(envfile)
    ENV = dict(kv.split("=", 1) for kv in raw.split("\0") if "=" in kv)
    for k in ("IMBO_WAFFO_PRIVATE_KEY", "IMBO_APPLE_PRIVATE_KEY"):
        SECRETS.append(ENV.get(k))
    CODE_AT.clear()
    ID.clear()
    for line in open(f"{L}/ids.tsv"):
        parts = line.split()
        if len(parts) >= 2:
            ID[parts[0]] = int(parts[1])
    APP.clear()
    for n in tokens:
        app_login(n)
    say(f"   setup: up {contacts}; alice {ID['alice']}, bob {ID['bob']}, carol {ID['carol']}")


def age(account, days=31):
    run([ENV["S"], "dev", "age-account", str(account), "--days", str(days)],
        env={"IMBO_DATA_DIR": f"{L}/server"})


def waffo_ok(out):
    return re.search(r"^200\b", out, re.M) is not None


def shape(s, plan, status_):
    return isinstance(s, dict) and s.get("plan") == plan and s.get("status") == status_ \
        and s.get("provider") == "apple"


# ---------- TC-261 ----------

def tc261():
    setup("alice:bob")
    A, B = ID["alice"], ID["bob"]
    age(A)
    ra, rb = billing("alice"), billing("bob")
    _, token = ae("token", str(A))
    aps = ra[1].get("app_store") or {}
    check(261, 1, ra[1]["mode"] == "test" and rb[1]["mode"] == "test"
          and aps.get("product_monthly") == "dev.imbo.app.monthly" and aps.get("product_yearly") == "dev.imbo.app.yearly"
          and aps.get("account_token") == token.strip()
          and aps.get("account_token") != (rb[1].get("app_store") or {}).get("account_token")
          and ra[1]["access"]["state"] == "expired",
          "both mode test; app_store products; alice's token = AE token, differs from bob's; alice expired",
          (ra, rb[1].get("app_store"), token.strip()))
    code, out = imbo("alice", "send", str(B), "T261")
    check(261, 2, "Subscription required" in out, "alice imbo send: Subscription required", (code, out))
    before = len(fake())
    r = tx(A, 2000000261, env="Xcode", submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    check(261, 3, r[0] == 200 and shape(s, "monthly", "active") and near(s["paid_until"], 30 * DAY)
          and len(fake()) == before,
          "Xcode transaction: 200 {monthly, active, apple}, paid_until 30 days on; no query to Apple",
          (r, fake()[before:]))
    b = billing("alice")[1]
    code, out = imbo("alice", "send", str(B), "T261b")
    check(261, 4, b["access"]["state"] == "paid" and shape(b.get("subscription"), "monthly", "active")
          and "Sent" in out, "alice paid, subscription as in step 3; send: Sent", (b, out))
    r = tx(A, 2000000261, env="Xcode", submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    rows = sql("SELECT count(*), max(mode) FROM subscriptions WHERE id = '2000000261'")
    check(261, 5, r[0] == 200 and shape(s, "monthly", "active") and near(s["paid_until"], 30 * DAY)
          and rows == [(1, "test")], "again: 200, same; one row in mode test", (r, rows))
    r = tx(A, 2000000261, "--id", "2000000262", product="yearly", env="Xcode", expires="+365d", submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    check(261, 6, r[0] == 200 and shape(s, "yearly", "active") and near(s["paid_until"], 365 * DAY),
          "later transaction 2000000262 yearly: plan yearly, paid_until 365 days on", r)
    server({"IMBO_WAFFO_ENV": "prod"})
    r = tx(A, 2000000263, env="Xcode", submit="alice")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000263'")
    check(261, 7, problem(r, 400, "invalid-request") and rows == [(0,)],
          "live mode: Xcode transaction 400 invalid-request, not recorded", (brief(r), rows))
    b = billing("alice")[1]
    check(261, 8, b["mode"] == "live" and b["access"]["state"] == "expired",
          "live: mode live, alice expired (test-mode subscription doesn't count)", b)
    server()


# ---------- TC-262 ----------

def tc262():
    setup("alice:bob")
    A, B = ID["alice"], ID["bob"]
    cookie = web_login("alice@imbo.test", f"{L}/alice.jar")
    status(2000000270, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    _, jws = tx(A, 2000000270)
    jws = jws.strip()
    body = {"signed_transaction": jws}
    r1 = call("POST", "/v1/billing/apple/transactions", cookie=cookie, origin=WEB, body=body)
    r2 = call("POST", "/v1/billing/apple/transactions", token=device_token("alice"), body=body)
    r3 = call("POST", "/v1/billing/apple/transactions", body=body)
    check(262, 1, problem(r1, 403, "forbidden") and problem(r2, 403, "forbidden") and problem(r3, 401, "unauthorized"),
          "web cookie 403, device token 403, no sign-in 401", (brief(r1), brief(r2), brief(r3)))
    r = call("POST", "/v1/billing/apple/transactions", token=APP["alice"], body={})
    check(262, 2, problem(r, 400, "invalid-request"), "App token, body {}: 400 invalid-request", brief(r))
    before = len(fake())
    rs = [tx(A, 2000000270, "--bad-signature", submit="alice"),
          tx(A, 2000000270, "--bundle", "com.example.other", submit="alice"),
          tx(A, 2000000270, product="com.example.other.monthly", submit="alice"),
          tx(A, 2000000270, env="Production", submit="alice")]
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000270'")
    check(262, 3, all(problem(r, 400, "invalid-request") for r in rs) and rows == [(0,)] and len(fake()) == before,
          "bad signature, other bundle, other product, Production: all 400; not recorded; no query",
          ([brief(r) for r in rs], rows, fake()[before:]))
    r = tx(A, 2000000270, "--token-of", str(B), submit="alice")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000270'")
    check(262, 4, problem(r, 409, "other-account") and rows == [(0,)], "bob's token: 409 other-account, not recorded",
          (brief(r), rows))
    before = len(fake())
    r = tx(A, 2000000270, submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    check(262, 5, r[0] == 200 and shape(s, "monthly", "active") and fake()[before:] == ["sandbox 2000000270 200"],
          "alice's own: 200 {monthly, active, apple}; query logged sandbox 2000000270 200", (r, fake()[before:]))
    r = tx(B, 2000000270, "--id", "2000000271", submit="bob")
    rows = sql("SELECT account_id FROM subscriptions WHERE id = '2000000270'")
    bb = billing("bob")[1]
    check(262, 6, problem(r, 409, "other-account") and rows == [(A,)] and "subscription" not in bb,
          "bob hands in alice's subscription: 409 other-account; still alice's; bob has none", (brief(r), rows, bb))
    server(drop=[k for k in ENV if k.startswith("IMBO_APPLE_")])
    r = tx(A, 2000000270, submit="alice")
    bb = billing("alice")[1]
    check(262, 7, problem(r, 404, "not-found") and "app_store" not in bb,
          "no Apple configured: 404 not-found; billing has no app_store", (brief(r), bb))
    server()


# ---------- TC-263 ----------

def tc263():
    setup("alice:bob,alice:carol")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    age(A)
    age(B)

    def st(n, *more):
        return status(n, "--env", "Sandbox", *more)

    st(2000000281, "--account", str(A), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    before = len(fake())
    r = tx(A, 2000000281, submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    check(263, 1, r[0] == 200 and shape(s, "monthly", "active") and near(s["paid_until"], 30 * DAY)
          and fake()[before:] == ["sandbox 2000000281 200"],
          "Apple says active, renewing: 200 active, paid_until 30 days on; one query", (r, fake()[before:]))
    paid = s and s["paid_until"]
    st(2000000281, "--auto-renew", "off")
    r = tx(A, 2000000281, submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    b = billing("alice")[1]
    check(263, 2, r[0] == 200 and s["status"] == "scheduled_cancel" and s["paid_until"] == paid
          and b["access"]["state"] == "paid", "auto-renew off: scheduled_cancel, paid_until same, alice paid", (r, b))
    out = []
    for state in ("grace", "retry"):
        st(2000000281, "--state", state)
        r = tx(A, 2000000281, submit="alice")
        out.append(r)
    check(263, 3, all(r[0] == 200 and r[1]["subscription"]["status"] == "past_due"
                      and r[1]["subscription"]["paid_until"] == paid for r in out),
          "grace and billing retry: past_due, paid_until same", out)
    st(2000000281, "--state", "revoked")
    r = tx(A, 2000000281, submit="alice")
    b = billing("alice")[1]
    code, sent = imbo("alice", "send", str(B), "T263")
    check(263, 4, r[0] == 200 and b["access"]["state"] == "expired" and b["subscription"]["status"] == "past_due"
          and "Subscription required" in sent, "revoked: 200; alice expired, status stays past_due; send refused",
          (r, b, sent))
    st(2000000282, "--account", str(A), "--state", "expired", "--expires", "-1d")
    r = tx(A, 2000000282, submit="alice")
    s = (r[1] or {}).get("subscription") if isinstance(r[1], dict) else None
    check(263, 5, r[0] == 200 and s["status"] == "canceled" and near(s["paid_until"], -DAY),
          "expired at Apple: canceled, paid_until a day ago", r)
    st(2000000283, "--account", str(A), "--state", "active", "--auto-renew", "on", "--expires", "+30d", "--fail", "500")
    r = tx(A, 2000000283, submit="alice")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000283'")
    check(263, 6, problem(r, 502, "payment-provider-failed") and rows == [(0,)],
          "Apple 500: 502 payment-provider-failed, not recorded", (brief(r), rows))
    st(2000000283, "--fail", "slow")
    t0 = time.time()
    r = tx(A, 2000000283, submit="alice")
    took = time.time() - t0
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000283'")
    check(263, 7, problem(r, 502, "payment-provider-failed") and 9 <= took <= 12 and rows == [(0,)],
          "Apple slow: 502 after 9-12 s, not recorded", (brief(r), round(took, 1), rows))
    st(2000000283, "--fail", "off")
    r = tx(A, 2000000283, submit="alice")
    n = notify("DID_CHANGE_RENEWAL_STATUS", 2000000283, "--subtype", "AUTO_RENEW_DISABLED", "--signed", "-1m")
    o = outcome()
    s = sub_of("alice")
    row = sql("SELECT status FROM subscriptions WHERE id = '2000000283'")
    check(263, 8, r[0] == 200 and r[1]["subscription"]["status"] == "active" and n[0] == 200 and o == "stale"
          and row == [("active",)], "transaction 200 active; notice signed a minute before the query: stale, unchanged",
          (r, n, o, row, s))
    n = notify("DID_CHANGE_RENEWAL_STATUS", 2000000283, "--subtype", "AUTO_RENEW_DISABLED", "--signed", "+0m")
    o = outcome()
    row = sql("SELECT status FROM subscriptions WHERE id = '2000000283'")
    check(263, 9, n[0] == 200 and o == "applied" and row == [("scheduled_cancel",)],
          "same notice signed now: applied, scheduled_cancel", (n, o, row))
    pull = {"DATABASE_URL": pgurl(), "IMBO_MAIL_LOG_ONLY": "1", "IMBO_DEV": "1"}
    st(2000000289, "--account", str(B), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    before = len(fake())
    code, out = run([ENV["S"], "billing", "pull", "2000000289"], env=pull)
    b = billing("bob")[1]
    check(263, 10, code == 0 and b["access"]["state"] == "paid" and shape(b.get("subscription"), "monthly", "active")
          and fake()[before:] == ["sandbox 2000000289 200"],
          "billing pull 2000000289: exit 0; bob paid, {monthly, active, apple}; one query", (code, out[-300:], b))
    st(2000000289, "--auto-renew", "off")
    code, out = run([ENV["S"], "billing", "pull", "2000000289"], env=pull)
    b = billing("bob")[1]
    check(263, 11, code == 0 and b["subscription"]["status"] == "scheduled_cancel",
          "pull again after auto-renew off: scheduled_cancel", (code, out[-300:], b))
    code, out = run([ENV["S"], "billing", "pull", "2000000299"], env=pull)
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000299'")
    check(263, 12, code != 0 and "Apple has no subscription" in out and rows == [(0,)],
          "pull an ID Apple doesn't know: non-zero, says Apple has none, not recorded", (code, out[-300:], rows))
    st(2000000289, "--fail", "500")
    code, out = run([ENV["S"], "billing", "pull", "2000000289"], env=pull)
    b = billing("bob")[1]
    check(263, 13, code != 0 and b["subscription"]["status"] == "scheduled_cancel",
          "pull while Apple fails: non-zero, bob unchanged", (code, out[-300:], b))
    code, out = run([os.path.join(HERE, "waffo-event.py"), "subscription.activated", "--order", "ORD_t263",
                     "--account", str(C), "--period-end", "+30d"])
    st(2000000284, "--account", str(C), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    r = tx(C, 2000000284, submit="carol")
    dup = [ln for ln in log().splitlines() if 'event="duplicate_subscription"' in ln]
    rows = sql(f"SELECT count(*) FROM subscriptions WHERE account_id = {C}")
    check(263, 14, waffo_ok(out) and r[0] == 200 and dup and f"account_id={C}" in dup[-1]
          and rows == [(2,)], "carol pays on the web and in the App: both 200, duplicate_subscription for carol, two rows",
          (out[-200:], r[0], dup[-1:] if dup else None, rows))


# ---------- TC-287 (TC-264 with GRACE_PERIOD_EXPIRED as past_due) ----------

def tc287():
    setup("alice:bob,alice:carol")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    age(B)

    def send(kind, *more, original=2000000291):
        r = notify(kind, original, *more)
        return r, outcome()

    r, o = send("SUBSCRIBED", "--subtype", "INITIAL_BUY", "--account", str(B), "--expires", "+30d")
    b = billing("bob")[1]
    check(287, 1, r[0] == 200 and o == "applied" and b["access"]["state"] == "paid"
          and shape(b.get("subscription"), "monthly", "active") and near(b["subscription"]["paid_until"], 30 * DAY),
          "SUBSCRIBED by bob's token: applied, paid, {monthly, active, apple}, 30 days on", (r, o, b))
    send("DID_RENEW", "--product", "yearly", "--expires", "+395d")
    s = sub_of("bob")
    check(287, 2, s["plan"] == "yearly" and near(s["paid_until"], 395 * DAY), "DID_RENEW yearly: yearly, 395 days on", s)
    paid = s["paid_until"]
    send("DID_FAIL_TO_RENEW", "--subtype", "GRACE_PERIOD")
    b = billing("bob")[1]
    check(287, 3, b["subscription"]["status"] == "past_due" and b["subscription"]["paid_until"] == paid
          and b["access"]["state"] == "paid", "DID_FAIL_TO_RENEW: past_due, paid_until same, still paid", b)
    send("DID_RENEW", "--product", "yearly", "--expires", "+400d")
    s = sub_of("bob")
    check(287, 4, s["status"] == "active" and near(s["paid_until"], 400 * DAY), "DID_RENEW: active, 400 days on", s)
    paid = s["paid_until"]
    send("DID_CHANGE_RENEWAL_STATUS", "--subtype", "AUTO_RENEW_DISABLED")
    s = sub_of("bob")
    check(287, 5, s["status"] == "scheduled_cancel" and s["paid_until"] == paid,
          "auto-renew disabled: scheduled_cancel, paid_until same", s)
    send("DID_CHANGE_RENEWAL_STATUS", "--subtype", "AUTO_RENEW_ENABLED")
    s = sub_of("bob")
    check(287, 6, s["status"] == "active" and s["paid_until"] == paid, "auto-renew enabled: active, paid_until same", s)
    send("REFUND")
    b = billing("bob")[1]
    code, out = imbo("bob", "send", str(A), "T264")
    check(287, 7, b["access"]["state"] == "expired" and b["subscription"]["status"] == "active"
          and "Subscription required" in out, "REFUND: bob expired, status stays active; send refused", (b, out))
    send("REFUND_REVERSED")
    b = billing("bob")[1]
    code, out = imbo("bob", "send", str(A), "T264b")
    check(287, 8, b["access"]["state"] == "paid" and "Sent" in out, "REFUND_REVERSED: paid again; send: Sent", (b, out))
    res = [send("TEST"), send("DID_CHANGE_RENEWAL_PREF", "--subtype", "DOWNGRADE"), send("OFFER_REDEEMED"),
           send("PRICE_INCREASE")]
    s2 = sub_of("bob")
    check(287, 9, all(r[0] == 200 and o == "ignored" for r, o in res) and s2 == s,
          "TEST, DID_CHANGE_RENEWAL_PREF, OFFER_REDEEMED, PRICE_INCREASE: 200 ignored; unchanged", (res, s2))
    send("EXPIRED", "--subtype", "VOLUNTARY")
    s = sub_of("bob")
    check(287, 10, s["status"] == "canceled" and s["paid_until"] == paid, "EXPIRED: canceled, paid_until same", s)
    r1, o1 = send("SUBSCRIBED", "--subtype", "INITIAL_BUY", "--account", str(C), "--expires", "+30d",
                  original=2000000292)
    paid = sub_of("carol")["paid_until"]
    r2, o2 = send("GRACE_PERIOD_EXPIRED", original=2000000292)
    b = billing("carol")[1]
    s = b["subscription"]
    check(287, 11, r1[0] == 200 and o1 == "applied" and r2[0] == 200 and o2 == "applied" and s["status"] == "past_due"
          and s["paid_until"] == paid and b["access"]["state"] == "paid",
          "carol SUBSCRIBED then GRACE_PERIOD_EXPIRED: both applied; past_due, paid_until same, still paid",
          (r1, o1, r2, o2, b))


# ---------- TC-265 ----------

def tc265():
    setup("alice:bob")
    A, B = ID["alice"], ID["bob"]
    notify("SUBSCRIBED", 2000000301, "--subtype", "INITIAL_BUY", "--account", str(A), "--expires", "+30d")

    def paid():
        return sub_of("alice")["paid_until"]

    r1 = notify("DID_RENEW", 2000000301, "--expires", "+60d", "--bad-signature")
    r2 = notify("DID_RENEW", 2000000301, "--expires", "+60d", "--bundle", "com.example.other")
    check(265, 1, problem(r1, 401, "unauthorized") and problem(r2, 401, "unauthorized") and near(paid(), 30 * DAY),
          "bad signature, other bundle: 401 unauthorized; paid_until still 30 days on", (brief(r1), brief(r2), paid()))
    u = "00000000-0000-4000-8000-000000000265"
    r1 = notify("DID_RENEW", 2000000301, "--expires", "+60d", "--uuid", u)
    o1, p1 = outcome(), paid()
    r2 = notify("DID_RENEW", 2000000301, "--expires", "+90d", "--uuid", u)
    o2, p2 = outcome(), paid()
    rows = sql(f"SELECT count(*) FROM billing_events WHERE id = 'apple:{u}'")
    check(265, 2, r1[0] == 200 and r2[0] == 200 and o1 == "applied" and near(p1, 60 * DAY) and o2 == "duplicate"
          and p2 == p1 and rows == [(1,)], "same UUID twice: applied 60 days, then duplicate unchanged; stored once",
          (r1, o1, p1, r2, o2, p2, rows))
    r = notify("DID_RENEW", 2000000301, "--expires", "+10d", "--signed", "-2h")
    check(265, 3, r[0] == 200 and outcome() == "stale" and paid() == p1, "signed 2 h ago: stale, unchanged",
          (r, outcome(), paid()))
    r = notify("DID_RENEW", 2000000301, "--expires", "+70d", env="Production")
    check(265, 4, r[0] == 200 and outcome() == "ignored" and paid() == p1, "Production notice on a test server: ignored",
          (r, outcome(), paid()))
    r = notify("SUBSCRIBED", 2000000302, "--subtype", "INITIAL_BUY", "--no-token", "--expires", "+30d")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000302'")
    check(265, 5, r[0] == 200 and outcome() == "unmapped" and rows == [(0,)], "no token, not recorded: unmapped",
          (r, outcome(), rows))
    r = notify("DID_RENEW", 2000000301, "--expires", "+80d", "--token-of", str(B))
    bb = billing("bob")[1]
    check(265, 6, r[0] == 200 and outcome() == "applied" and near(paid(), 80 * DAY) and "subscription" not in bb,
          "bob's token on alice's subscription: applied to alice (80 days); bob has none", (r, outcome(), paid(), bb))
    server(drop=[k for k in ENV if k.startswith("IMBO_APPLE_")])
    r = notify("DID_RENEW", 2000000301, "--expires", "+90d")
    check(265, 7, problem(r, 404, "not-found"), "no Apple configured: 404 not-found", brief(r))
    server()


# ---------- TC-266 ----------

def tc266():
    setup("alice:bob", "export IMBO_WAFFO_ENV=prod IMBO_REVIEW_EMAIL=review@imbo.test IMBO_REVIEW_CODE=246810",
          tokens=("alice", "bob"))
    A, B = ID["alice"], ID["bob"]
    call("POST", "/v1/auth/code", body={"email": "review@imbo.test"})
    r = call("POST", "/v1/app/session", body={"email": "review@imbo.test", "code": "246810", "name": "reviewer",
                                               "platform": "ios"})
    APP["review"] = r[1]["token"]
    SECRETS.append(APP["review"])
    with open(f"{L}/review.app", "w") as f:
        f.write(APP["review"] + "\n")
    R = call("GET", "/v1/me", token=APP["review"])[1]["account"]["id"]
    age(R)
    age(B)
    say(f"   review account {R}")
    b = billing("review")[1]
    check(266, 1, b["mode"] == "live" and b["access"]["state"] == "expired", "review: live, expired", b)

    def pair(account, n, name, env="Sandbox"):
        status(n, "--account", str(account), "--env", env, "--state", "active", "--auto-renew", "on", "--expires", "+30d")
        return tx(account, n, env=env, submit=name)

    before = len(fake())
    r = pair(R, 2000000311, "review")
    b = billing("review")[1]
    rows = sql("SELECT mode FROM subscriptions WHERE id = '2000000311'")
    check(266, 2, r[0] == 200 and r[1]["subscription"]["status"] == "active" and b["access"]["state"] == "paid"
          and rows == [("test",)] and fake()[before:] == ["sandbox 2000000311 200"],
          "review's Sandbox purchase on live: 200 active, paid, mode test, asked Apple's sandbox",
          (r, b, rows, fake()[before:]))
    r = pair(B, 2000000312, "bob")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000312'")
    check(266, 3, problem(r, 400, "invalid-request") and rows == [(0,)], "bob's Sandbox purchase on live: 400, not recorded",
          (brief(r), rows))
    before = len(fake())
    r = pair(B, 2000000313, "bob", env="Production")
    b = billing("bob")[1]
    rows = sql("SELECT mode FROM subscriptions WHERE id = '2000000313'")
    check(266, 4, r[0] == 200 and b["access"]["state"] == "paid" and rows == [("live",)]
          and fake()[before:] == ["production 2000000313 200"],
          "bob's Production purchase: 200, paid, mode live, asked production", (r, b, rows, fake()[before:]))
    r = notify("DID_RENEW", 2000000311, "--expires", "+60d")
    s = sub_of("review")
    check(266, 5, r[0] == 200 and outcome() == "applied" and near(s["paid_until"], 60 * DAY),
          "review's Sandbox notice on live: applied, 60 days on", (r, outcome(), s))
    paid = s["paid_until"]
    r = notify("SUBSCRIBED", 2000000314, "--subtype", "INITIAL_BUY", "--account", str(B), "--expires", "+30d")
    rows = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000314'")
    check(266, 6, r[0] == 200 and outcome() == "ignored" and rows == [(0,)], "bob's Sandbox notice on live: ignored",
          (r, outcome(), rows))
    r = tx(R, 2000000315, env="Xcode", submit="review")
    check(266, 7, problem(r, 400, "invalid-request"), "Xcode transaction on live, review too: 400", brief(r))
    pid = int(open(f"{L}/server.pid").read())
    os.kill(pid, signal.SIGTERM)
    while True:
        try:
            os.kill(pid, 0)
            time.sleep(0.2)
        except ProcessLookupError:
            break
    env = {k: v for k, v in ENV.items() if k != "IMBO_DEV"}
    env.update({"IMBO_DATA_DIR": f"{L}/server", "IMBO_LISTEN": "127.0.0.1:18080", "IMBO_MAIL_LOG_ONLY": "1"})
    with open(f"{L}/server.log", "a") as out:
        p = subprocess.Popen([ENV["S"]], env=env, cwd=L, stdout=out, stderr=out, start_new_session=True)
    with open(f"{L}/server.pid", "w") as f:
        f.write(f"{p.pid}\n")
    wait_health()
    before = len(fake())
    r = pair(R, 2000000316, "review")
    check(266, 8, problem(r, 400, "invalid-request") and not any("2000000316" in x for x in fake()[before:]),
          "without IMBO_DEV the local root isn't trusted: 400, no query for 2000000316", (brief(r), fake()[before:]))
    r = notify("DID_RENEW", 2000000311, "--expires", "+90d")
    s = sub_of("review")
    check(266, 9, problem(r, 401, "unauthorized") and s["paid_until"] == paid,
          "notice signed by the local root: 401; review unchanged", (brief(r), s))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# ---------- TC-267 ----------

def tc267():
    setup("alice:bob,alice:carol")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    status(2000000321, "--account", str(B), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    tx(B, 2000000321, submit="bob")

    def delete(name):
        return call("DELETE", "/v1/account", token=APP[name], body={"confirm": "DELETE"})

    before = len(fake())
    r = delete("bob")
    me = call("GET", "/v1/me", token=APP["bob"])
    check(267, 1, problem(r, 409, "app-store-renewing") and me[0] == 200 and fake()[before:] == ["sandbox 2000000321 200"],
          "still renewing: 409 app-store-renewing, bob stays; one query", (brief(r), me[0], fake()[before:]))
    status(2000000321, "--fail", "500")
    r = delete("bob")
    me = call("GET", "/v1/me", token=APP["bob"])
    check(267, 2, problem(r, 502, "payment-provider-failed") and me[0] == 200, "Apple 500: 502, bob stays",
          (brief(r), me[0]))
    status(2000000321, "--fail", "slow")
    t0 = time.time()
    r = delete("bob")
    took = time.time() - t0
    me = call("GET", "/v1/me", token=APP["bob"])
    check(267, 3, problem(r, 502, "payment-provider-failed") and 9 <= took <= 12 and me[0] == 200,
          "Apple slow: 502 after 9-12 s, bob stays", (brief(r), round(took, 1), me[0]))
    status(2000000321, "--fail", "off", "--auto-renew", "off")
    r = delete("bob")
    me = call("GET", "/v1/me", token=APP["bob"])
    code, out = imbo("alice", "contacts")
    check(267, 4, r[0] == 204 and me[0] == 401 and "bob" not in out, "auto-renew off: 204; bob signed out; not a contact",
          (brief(r), me[0], out[-300:]))
    status(2000000322, "--account", str(C), "--env", "Sandbox", "--state", "expired", "--expires", "-1d")
    t = tx(C, 2000000322, expires="-1d", submit="carol")
    r = delete("carol")
    lines = [x for x in fake() if "2000000322" in x]
    check(267, 5, t[0] == 200 and t[1]["subscription"]["status"] == "canceled" and r[0] == 204
          and lines == ["sandbox 2000000322 200"],
          "carol's canceled subscription: transaction 200 canceled; delete 204 without asking Apple", (t, brief(r), lines))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# ---------- TC-268 ----------

LETTER = (
    "Your Imbo account (ID {id}) has been suspended. You can no longer sign in or send messages.\n\n"
    "Your Imbo subscription is billed by Apple through the App Store, and we can't cancel it for you. "
    "To stop future charges, open Settings on your iPhone, tap your name, tap Subscriptions, choose Imbo, "
    "then tap Cancel Subscription.\n\n"
    "To ask Apple for a refund, go to https://reportaproblem.apple.com.\n\n"
    "If you think this is a mistake, write to support@imbo.dev."
)


def letters(email):
    """Bodies of the suspension letters to `email` in the server log."""
    out = []
    dec = json.JSONDecoder()
    for ln in log().splitlines():
        m = re.search(r"mail-log-only to=(\S+) subject=", ln)
        if not m or m.group(1) != email:
            continue
        subject, end = dec.raw_decode(ln, m.end())
        body, _ = dec.raw_decode(ln, end + len(" body="))
        if subject == "Your Imbo account has been suspended":
            out.append(body)
    return out


def tc268():
    setup("alice:bob,alice:carol", "export IMBO_ADMIN_EMAILS=admin@imbo.test IMBO_REPORT_EMAIL=reports@imbo.test")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    admin = web_login("admin@imbo.test", f"{L}/admin.jar", "admin")
    status(2000000331, "--account", str(B), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    tx(B, 2000000331, submit="bob")

    def ban(x):
        return call("POST", f"/v1/admin/accounts/{x}/ban", cookie=admin, origin=WEB)

    refused = log().count("waffo refused")
    r = ban(B)
    got = letters("bob@imbo.test")
    check(268, 1, r[0] == 200 and r[1]["app_store_subscription"] is True and got == [LETTER.format(id=B)]
          and log().count("waffo refused") == refused,
          "ban bob: 200, app_store_subscription true; one letter word for word; Waffo not called",
          (brief(r) if r[0] != 200 else r[1].get("app_store_subscription"), got, log().count("waffo refused") - refused))
    r = ban(C)
    check(268, 2, r[0] == 200 and r[1]["app_store_subscription"] is False and letters("carol@imbo.test") == [],
          "ban carol: false, no letter", (r[0], r[1].get("app_store_subscription"), letters("carol@imbo.test")))
    r = ban(B)
    check(268, 3, r[0] == 200 and r[1]["app_store_subscription"] is True and len(letters("bob@imbo.test")) == 1,
          "ban bob again: true, still one letter", (r[0], r[1].get("app_store_subscription"),
                                                    len(letters("bob@imbo.test"))))
    r = notify("DID_RENEW", 2000000331, "--expires", "+60d")
    rows = sql("SELECT paid_until FROM subscriptions WHERE id = '2000000331'")
    check(268, 4, r[0] == 200 and outcome() == "applied" and near(rows[0][0], 60 * DAY),
          "DID_RENEW for banned bob: applied, 60 days on", (r, outcome(), rows))
    r = call("DELETE", f"/v1/admin/accounts/{B}", cookie=admin, origin=WEB, body={"confirm": "DELETE"})
    lines = [x for x in fake() if "2000000331" in x]
    check(268, 5, r[0] == 204 and lines == ["sandbox 2000000331 200"],
          "delete banned bob: 204 without asking Apple", (brief(r), lines))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# ---------- TC-269 ----------

M9 = "/tmp/imbo-m9"
M9_URL = "http://127.0.0.1:18083"


def m9_start(binary):
    env = {**ENV, "IMBO_DATA_DIR": M9, "IMBO_LISTEN": "127.0.0.1:18083", "IMBO_MAIL_LOG_ONLY": "1", "IMBO_DEV": "1"}
    out = open(f"{M9}.log", "a")
    p = subprocess.Popen([binary], env=env, cwd="/tmp", stdout=out, stderr=out, start_new_session=True)
    return p, wait_health(M9_URL)


def m9_stop(p):
    p.send_signal(signal.SIGTERM)
    p.wait(timeout=30)


def columns():
    return [r[0] for r in sql("SELECT column_name FROM information_schema.columns WHERE table_name = 'subscriptions' "
                              "ORDER BY ordinal_position", M9)]


def tc269():
    setup("alice:bob")
    shutil.rmtree(M9, ignore_errors=True)
    p, _ = m9_start(OLD)
    run([OLD, "dev", "seed", "--homes", f"{M9}/h"], env={"IMBO_DATA_DIR": M9})
    a9 = int(re.search(r"^account_id = (\d+)", open(f"{M9}/h/alice/credentials").read(), re.M).group(1))
    code, out = run([os.path.join(HERE, "waffo-event.py"), "subscription.activated", "--order", "ORD_t269",
                     "--account", str(a9), "--period-end", "+30d", "--url", f"{M9_URL}/v1/billing/waffo/webhook"])
    cols = columns()
    m9_stop(p)
    check(269, 1, waffo_ok(out) and "provider" not in cols, "old build: notice 200; no provider column",
          (out[-200:], cols))
    p, health = m9_start(ENV["S"])
    cols = columns()
    rows = sql("SELECT id, provider FROM subscriptions", M9)
    ver = sql("SELECT version, success FROM _sqlx_migrations WHERE version = 9", M9)
    want = run([ENV["S"], "--version"])[1].split()[-1]
    check(269, 2, health.get("version") == want and "provider" in cols and rows == [("ORD_t269", "waffo")]
          and ver == [(9, True)], "new build: version, provider column, old row is waffo, migration 9 ran",
          (health, cols, rows, ver))
    cred = open(f"{M9}/h/bob/credentials").read()
    token = re.search(r'^token = "(.*)"', cred, re.M).group(1)
    SECRETS.append(token)
    b9 = int(re.search(r"^account_id = (\d+)", cred, re.M).group(1))
    r = ae("notify", "SUBSCRIBED", "--subtype", "INITIAL_BUY", "--original", "2000000341", "--account", str(b9),
           "--env", "Sandbox", "--expires", "+30d", "--url", M9_URL)
    rows = sql("SELECT provider FROM subscriptions WHERE id = '2000000341'", M9)
    run([ENV["S"], "dev", "age-account", str(b9), "--days", "31"], env={"IMBO_DATA_DIR": M9})
    m9_stop(p)
    check(269, 3, r[0] == 200 and rows == [("apple",)], "App Store notice: 200, provider apple", (r, rows))
    p, health = m9_start(OLD)
    b = call("GET", "/v1/billing", token=token, base=M9_URL)
    n = call("POST", "/v1/billing/apple/notifications", body={}, base=M9_URL)
    m9_stop(p)
    check(269, 4, health.get("status") == "ok" and b[0] == 200 and b[1]["access"]["state"] == "paid" and n[0] == 404,
          "old build again: up; bob paid; notifications 404", (health, b, n[0]))
    shutil.rmtree(M9, ignore_errors=True)
    try:
        os.remove(f"{M9}.log")
    except FileNotFoundError:
        pass


def tc282():
    setup("alice:bob")
    A = ID["alice"]
    cookie = web_login("alice@imbo.test", f"{L}/alice.jar")
    status(2000000270, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    _, jws = tx(A, 2000000270)
    body = {"signed_transaction": jws.strip()}
    want = "Only the Imbo App can do this."
    r = call("POST", "/v1/billing/apple/transactions", token=device_token("alice"), body=body)
    check(282, 1, problem(r, 403, "forbidden") and r[1].get("detail") == want,
          f"device token: 403 forbidden, detail {want!r}", brief(r))
    r = call("POST", "/v1/billing/apple/transactions", cookie=cookie, origin=WEB, body=body)
    check(282, 2, problem(r, 403, "forbidden") and r[1].get("detail") == want,
          f"web cookie with Origin: 403 forbidden, detail {want!r}", brief(r))


def tc285():
    setup("alice:bob")
    A, B = ID["alice"], ID["bob"]
    age(A)
    age(B)

    def revoked(n):
        return sql(f"SELECT revoked_at IS NOT NULL FROM subscriptions WHERE id = '{n}'")

    status(2000000361, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    r = tx(A, 2000000361, submit="alice")
    b = billing("alice")[1]
    check(285, 1, r[0] == 200 and b["access"]["state"] == "paid", "alice hands in 2000000361: 200, paid", (brief(r), b))
    r = notify("REFUND_REVERSED", 2000000361, "--signed", "-1h")
    o = outcome()
    b = billing("alice")[1]
    check(285, 2, r[0] == 200 and o == "applied" and b["access"]["state"] == "paid",
          "REFUND_REVERSED signed an hour ago: 200 applied; still paid", (r, o, b))
    r = notify("REFUND", 2000000361, "--signed", "-2h")
    o = outcome()
    b = billing("alice")[1]
    rv = revoked(2000000361)
    check(285, 3, r[0] == 200 and o == "stale" and b["access"]["state"] == "paid" and rv == [(False,)],
          "REFUND signed two hours ago, before the reversal: 200 stale; still paid; no refund time", (r, o, b, rv))
    r = notify("REFUND", 2000000361)
    o = outcome()
    b = billing("alice")[1]
    check(285, 4, r[0] == 200 and o == "applied" and b["access"]["state"] == "expired",
          "REFUND signed now: 200 applied; alice expired", (r, o, b))
    status(2000000362, "--account", str(B), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    r = tx(B, 2000000362, submit="bob")
    check(285, 5, r[0] == 200 and billing("bob")[1]["access"]["state"] == "paid", "bob hands in 2000000362: 200, paid",
          brief(r))
    status(2000000362, "--fail", "late")
    args = [AE, "transaction", "--account", str(B), "--original", "2000000362", "--product", "monthly", "--env",
            "Sandbox", "--expires", "+30d", "--submit", "bob"]
    p = subprocess.Popen(args, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    time.sleep(2)
    r = notify("REFUND", 2000000362)
    o = outcome()
    out = p.communicate(timeout=60)[0].strip().splitlines()
    status(2000000362, "--fail", "off")
    b = billing("bob")[1]
    rv = revoked(2000000362)
    check(285, 6, r[0] == 200 and o == "applied" and out[:1] == ["200"] and b["access"]["state"] == "expired"
          and rv == [(True,)],
          "REFUND arrives while the server waits 5 s for Apple: both 200; the refund stays; bob expired",
          (r, o, out[:1], b, rv))


def tc286():
    setup("alice:bob", "export IMBO_WAFFO_ENV=prod IMBO_REVIEW_EMAIL=review@imbo.test IMBO_REVIEW_CODE=246810",
          tokens=("alice", "bob"))
    B = ID["bob"]
    call("POST", "/v1/auth/code", body={"email": "review@imbo.test"})
    r = call("POST", "/v1/app/session", body={"email": "review@imbo.test", "code": "246810", "name": "reviewer",
                                               "platform": "ios"})
    APP["review"] = r[1]["token"]
    SECRETS.append(APP["review"])
    R = call("GET", "/v1/me", token=APP["review"])[1]["account"]["id"]
    age(R)
    age(B)
    say(f"   review account {R}")
    pull = {"DATABASE_URL": pgurl(), "IMBO_MAIL_LOG_ONLY": "1", "IMBO_DEV": "1"}

    def mode(n):
        return sql(f"SELECT mode FROM subscriptions WHERE id = '{n}'")

    status(2000000391, "--account", str(R), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    before = len(fake())
    code, out = run([ENV["S"], "billing", "pull", "2000000391"], env=pull)
    b = billing("review")[1]
    check(286, 1, code == 0 and mode(2000000391) == [("test",)] and b["access"]["state"] == "paid"
          and fake()[before:] == ["production 2000000391 404", "sandbox 2000000391 200"],
          "pull the review account's unrecorded sandbox subscription: exit 0; mode test; review paid; "
          "asked production, then sandbox", (code, out[-300:], mode(2000000391), b, fake()[before:]))
    status(2000000392, "--account", str(B), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    code, out = run([ENV["S"], "billing", "pull", "2000000392"], env=pull)
    b = billing("bob")[1]
    check(286, 2, code != 0 and "not recorded" in out and mode(2000000392) == [] and b["access"]["state"] == "expired",
          "pull bob's unrecorded sandbox subscription: non-zero, not recorded; bob expired",
          (code, out[-300:], mode(2000000392), b))
    status(2000000393, "--account", str(B), "--env", "Production", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    before = len(fake())
    code, out = run([ENV["S"], "billing", "pull", "2000000393"], env=pull)
    b = billing("bob")[1]
    check(286, 3, code == 0 and mode(2000000393) == [("live",)] and b["access"]["state"] == "paid"
          and fake()[before:] == ["production 2000000393 200"],
          "pull bob's unrecorded production subscription: exit 0; mode live; bob paid; one query",
          (code, out[-300:], mode(2000000393), b, fake()[before:]))


def owner(n):
    return sql(f"SELECT account_id FROM subscriptions WHERE id = '{n}'")


def lapsed(account, n):
    """alice's subscription n, recorded as Apple has it: expired yesterday."""
    status(n, "--account", str(account), "--env", "Sandbox", "--state", "expired", "--auto-renew", "off",
           "--expires=-1d")
    return tx(account, n, submit="alice", expires="-1d")


def tc291():
    setup("alice:bob,alice:carol")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    for x in (A, B, C):
        age(x)
    status(2000000431, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    ra = tx(A, 2000000431, submit="alice")
    status(2000000431, "--account", str(B), "--expires", "+60d")
    rb = tx(B, 2000000431, "--id", "2000000432", submit="bob", expires="+60d")
    check(291, 1, ra[0] == 200 and problem(rb, 409, "other-account") and owner(2000000431) == [(A,)],
          "bob hands in alice's subscription that still grants: alice 200, bob 409 other-account; still alice's",
          (brief(ra), brief(rb), owner(2000000431)))
    r0 = lapsed(A, 2000000433)
    status(2000000433, "--account", str(B), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    r = tx(B, 2000000433, "--id", "2000000434", submit="bob")
    b = billing("bob")[1]
    check(291, 2, r0[0] == 200 and r[0] == 200 and shape((r[1] or {}).get("subscription"), "monthly", "active")
          and b["access"]["state"] == "paid" and owner(2000000433) == [(B,)],
          "alice's expired subscription, bob hands in his new transaction: 200 active; bob paid; now bob's",
          (brief(r0), brief(r), b.get("access"), owner(2000000433)))
    r0 = lapsed(A, 2000000435)
    r = notify("SUBSCRIBED", 2000000435, "--subtype", "RESUBSCRIBE", "--account", str(C), "--expires", "+30d")
    o = outcome()
    b = billing("carol")[1]
    check(291, 3, r0[0] == 200 and r[0] == 200 and o == "applied" and b["access"]["state"] == "paid"
          and shape(b.get("subscription"), "monthly", "active") and owner(2000000435) == [(C,)],
          "alice's expired subscription, SUBSCRIBED with carol's token: applied; carol paid; now carol's",
          (brief(r0), r, o, b.get("access"), b.get("subscription"), owner(2000000435)))
    r0 = lapsed(A, 2000000437)
    status(2000000437, "--account", str(C), "--state", "active", "--auto-renew", "on", "--expires", "+60d")
    code, out = run([ENV["S"], "billing", "pull", "2000000437"],
                    env={"DATABASE_URL": pgurl(), "IMBO_MAIL_LOG_ONLY": "1", "IMBO_DEV": "1"})
    check(291, 4, r0[0] == 200 and code == 0 and f"2000000437: recorded for account {C}" in out
          and owner(2000000437) == [(C,)],
          "alice's expired subscription, Apple's latest is carol's: pull records it for carol",
          (brief(r0), code, out[-300:], owner(2000000437)))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def tc292():
    setup("alice:bob", tokens=("alice", "bob"))
    A, B = ID["alice"], ID["bob"]
    age(A)
    age(B)
    status(2000000441, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    r = tx(A, 2000000441, "--no-token", submit="alice")
    b = billing("alice")[1]
    check(292, 1, r[0] == 200 and shape((r[1] or {}).get("subscription"), "monthly", "active")
          and b["access"]["state"] == "paid" and owner(2000000441) == [(A,)],
          "alice hands in a transaction without a token: 200 active; paid; alice's",
          (brief(r), b.get("access"), owner(2000000441)))
    r = tx(B, 2000000441, "--id", "2000000442", "--no-token", submit="bob")
    b = billing("bob")[1]
    check(292, 2, problem(r, 409, "other-account") and "subscription" not in b and owner(2000000441) == [(A,)],
          "bob hands in one without a token for alice's subscription: 409 other-account; still alice's",
          (brief(r), b.get("subscription"), owner(2000000441)))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def tc293():
    setup("alice:bob,alice:carol", "export IMBO_ADMIN_EMAILS=admin@imbo.test IMBO_REPORT_EMAIL=reports@imbo.test")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    for x in (A, B, C):
        age(x)
    admin = web_login("admin@imbo.test", f"{L}/admin.jar", "admin")

    def live(account, n, name):
        status(n, "--account", str(account), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
               "--expires", "+30d")
        return tx(account, n, submit=name)

    def delete(name):
        return call("DELETE", "/v1/account", token=APP[name], body={"confirm": "DELETE"})

    r = call("POST", "/v1/billing/apple/notifications", body={})
    check(293, 1, problem(r, 401, "unauthorized"), "notification body {}: 401 unauthorized", brief(r))
    r = notify("REFUND", 2000000451, "--account", str(A))
    o = outcome()
    n = sql("SELECT count(*) FROM subscriptions WHERE id = '2000000451'")
    check(293, 2, r[0] == 200 and o == "unmapped" and n == [(0,)],
          "REFUND for a subscription never recorded: 200 unmapped; nothing recorded", (r, o, n))
    status(2000000452, "--account", str(A), "--env", "Sandbox", "--state", "revoked", "--auto-renew", "off",
           "--expires", "+30d")
    r = tx(A, 2000000452, submit="alice")
    b = billing("alice")[1]
    rv = sql("SELECT revoked_at IS NOT NULL FROM subscriptions WHERE id = '2000000452'")
    check(293, 3, r[0] == 200 and ((r[1] or {}).get("subscription") or {}).get("status") == "canceled"
          and b["access"]["state"] == "expired" and rv == [(True,)],
          "first recorded already revoked: 200 canceled; alice expired; refund time set",
          (brief(r), b.get("access"), rv))
    r1 = live(B, 2000000453, "bob")
    r2 = notify("REFUND", 2000000453)
    o = outcome()
    r3 = delete("bob")
    lines = [x for x in fake() if "2000000453" in x]
    check(293, 4, r1[0] == 200 and r2[0] == 200 and o == "applied" and r3[0] == 204
          and lines == ["sandbox 2000000453 200"],
          "bob's refunded subscription: deleting doesn't ask Apple, 204", (brief(r1), r2, o, brief(r3), lines))
    r1 = live(C, 2000000454, "carol")
    r2 = notify("REFUND", 2000000454)
    r3 = call("POST", f"/v1/admin/accounts/{C}/ban", cookie=admin, origin=WEB)
    got = letters("carol@imbo.test")
    check(293, 5, r1[0] == 200 and r2[0] == 200 and r3[0] == 200
          and (r3[1] or {}).get("app_store_subscription") is False and got == [],
          "ban carol, whose subscription was refunded: app_store_subscription false; no letter",
          (brief(r1), r2, brief(r3) if r3[0] != 200 else r3[1].get("app_store_subscription"), got))
    r1 = live(A, 2000000455, "alice")
    server(drop=[k for k in ENV if k.startswith("IMBO_APPLE_")])
    r2 = delete("alice")
    lines = [x for x in fake() if "2000000455" in x]
    check(293, 6, r1[0] == 200 and r2[0] == 204 and lines == ["sandbox 2000000455 200"],
          "without Apple billing, deleting alice doesn't ask Apple: 204", (brief(r1), brief(r2), lines))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def revoked(n):
    return sql(f"SELECT revoked_at IS NOT NULL FROM subscriptions WHERE id = '{n}'")


def tc295():
    setup("alice:bob,alice:carol")
    A, C = ID["alice"], ID["carol"]
    for x in (A, ID["bob"], C):
        age(x)
    status(2000000461, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    r0 = tx(A, 2000000461, submit="alice")
    r = notify("REFUND", 2000000461, "--account", str(A))
    o = outcome()
    b = billing("alice")[1]
    check(295, 1, r0[0] == 200 and r[0] == 200 and o == "applied" and b["access"]["state"] == "expired",
          "alice's subscription refunded: applied; alice expired", (brief(r0), r, o, b.get("access")))
    r = notify("SUBSCRIBED", 2000000461, "--subtype", "RESUBSCRIBE", "--account", str(C), "--expires", "+30d")
    o = outcome()
    b = billing("carol")[1]
    check(295, 2, r[0] == 200 and o == "applied" and b["access"]["state"] == "paid"
          and shape(b.get("subscription"), "monthly", "active") and owner(2000000461) == [(C,)]
          and revoked(2000000461) == [(False,)],
          "SUBSCRIBED with carol's token: applied; carol paid; now carol's, without alice's refund",
          (r, o, b.get("access"), b.get("subscription"), owner(2000000461), revoked(2000000461)))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def tc296():
    setup("alice:bob,alice:carol")
    A, B, C = ID["alice"], ID["bob"], ID["carol"]
    for x in (A, B, C):
        age(x)
    r0 = lapsed(A, 2000000471)
    status(2000000471, "--account", str(B), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    r = tx(B, 2000000471, "--id", "2000000472", "--no-token", submit="bob")
    b = billing("bob")[1]
    check(296, 1, r0[0] == 200 and r[0] == 200 and shape((r[1] or {}).get("subscription"), "monthly", "active")
          and b["access"]["state"] == "paid" and owner(2000000471) == [(B,)],
          "Apple's latest is bob's, bob hands in one without a token: 200 active; bob paid; now bob's",
          (brief(r0), brief(r), b.get("access"), owner(2000000471)))
    r0 = lapsed(A, 2000000473)
    status(2000000473, "--account", str(C), "--state", "active", "--auto-renew", "on", "--expires", "+30d")
    r = tx(B, 2000000473, "--id", "2000000474", submit="bob")
    check(296, 2, r0[0] == 200 and problem(r, 409, "other-account") and owner(2000000473) == [(A,)],
          "Apple's latest is carol's, bob hands in his own: 409 other-account; still alice's",
          (brief(r0), brief(r), owner(2000000473)))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def tc297():
    setup("alice:bob", tokens=())
    D = device_token("alice")
    want = "Only the Imbo App can do this."

    def app_only(res):
        return problem(res, 403, "forbidden") and res[1].get("detail") == want

    r = call("DELETE", "/v1/app/session", token=D)
    check(297, 1, app_only(r), "computer token, DELETE /v1/app/session: 403, only the App", brief(r))
    r = call("PUT", "/v1/app/push", token=D, body={"push_token": "ExponentPushToken[t297]"})
    check(297, 2, app_only(r), "computer token, PUT /v1/app/push: 403, only the App", brief(r))
    r = call("DELETE", "/v1/app/push", token=D)
    check(297, 3, app_only(r), "computer token, DELETE /v1/app/push: 403, only the App", brief(r))
    cookie = web_login("bob@imbo.test", f"{L}/bob.jar")
    r = call("DELETE", "/v1/app/session", cookie=cookie, origin=WEB)
    check(297, 4, app_only(r), "web sign-in, DELETE /v1/app/session: 403, only the App", brief(r))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def tc298():
    setup("alice:bob", tokens=("alice",))
    A = ID["alice"]
    age(A)
    status(2000000481, "--account", str(A), "--env", "Sandbox", "--state", "active", "--auto-renew", "on",
           "--expires", "+30d")
    r0 = tx(A, 2000000481, submit="alice")
    r1 = notify("REFUND", 2000000481, "--account", str(A))
    before = billing("alice")[1]
    r = notify("DID_RENEW", 2000000481, "--account", str(A), "--expires", "+60d")
    o = outcome()
    b = billing("alice")[1]
    check(298, 1, r0[0] == 200 and r1[0] == 200 and before["access"]["state"] == "expired" and r[0] == 200
          and o == "applied" and b["access"]["state"] == "paid" and near(b["subscription"]["paid_until"], 60 * DAY)
          and revoked(2000000481) == [(False,)],
          "refunded, then DID_RENEW: expired in between; applied; paid until +60d; refund time cleared",
          (brief(r0), r1, before.get("access"), r, o, b.get("access"), b.get("subscription"), revoked(2000000481)))
    r1 = notify("REFUND", 2000000481, "--account", str(A))
    before = billing("alice")[1]
    r = notify("SUBSCRIBED", 2000000481, "--subtype", "RESUBSCRIBE", "--account", str(A), "--expires", "+90d")
    o = outcome()
    b = billing("alice")[1]
    check(298, 2, r1[0] == 200 and before["access"]["state"] == "expired" and r[0] == 200 and o == "applied"
          and b["access"]["state"] == "paid" and revoked(2000000481) == [(False,)],
          "refunded again, then SUBSCRIBED: expired in between; applied; paid; refund time cleared",
          (r1, before.get("access"), r, o, b.get("access"), revoked(2000000481)))
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# ---------- main ----------

def write(run_id, tc, header):
    path = os.path.join(os.environ.get("EVIDENCE_DIR", "docs/test-runs"), f"{run_id.lower()}-tc-{tc}.txt")
    with open(path, "w") as f:
        f.write(f"{run_id} TC-{tc}" + (f" · {header}" if header else "") + "\n")
        f.write("\n".join(OUT) + "\n")
    OUT.clear()


if __name__ == "__main__":
    run_id = sys.argv[1]
    only = set(sys.argv[2:])
    header = os.environ.get("RUN_HEADER", "")
    cases = [("261", tc261), ("262", tc262), ("263", tc263), ("265", tc265), ("266", tc266), ("267", tc267),
             ("268", tc268), ("269", tc269), ("282", tc282), ("285", tc285), ("286", tc286), ("287", tc287),
             ("291", tc291), ("292", tc292), ("293", tc293), ("295", tc295), ("296", tc296), ("297", tc297),
             ("298", tc298)]
    for n, case in cases:
        if only and n not in only:
            continue
        try:
            case()
        except Exception:
            check(n, "-", False, "crashed", traceback.format_exc()[-900:])
        write(run_id, n, header)
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
