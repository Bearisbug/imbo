#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["psycopg[binary]>=3.2"]
# ///
"""TC-516 and TC-517 (docs/TEST.md, 支付宝时长包) against the local server,
with real HTTP requests, the fake Alipay of alipay-fake.py and real database
queries.

    . crates/scripts/local-env.sh
    crates/scripts/alipay-tests.py <RUN-ID> [TC numbers…]

Each case builds its own environment as its 前置 says: `local-env.sh down`,
then billing-env.sh, alipay-env.sh, `local-env.sh up …`, App tokens and web
sign-ins. Prints PASS or FAIL for every step and writes the lines to
docs/test-runs/<run>-tc-NNN.txt (EVIDENCE_DIR overrides the directory;
RUN_HEADER goes into the first line); exits 1 when any step failed. Leaves
the environment up; `local-env.sh down` ends it.

Tokens, cookies and codes are never written out, nor are the signatures of
orders.
"""

import datetime as dt
import json
import os
import re
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
AF = os.path.join(HERE, "alipay-fake.py")
U = "http://127.0.0.1:18080"
WEB = "https://app.imbo.dev"
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
BASE = dict(os.environ)
ENV = dict(BASE)
FAILS = 0
OUT = []
SECRETS = []
ID = {}
APP = {}
JAR = {}


# ---------- plumbing ----------

def hide(text):
    text = str(text)
    for s in SECRETS:
        if s:
            text = text.replace(s, "<secret>")
    return re.sub(r"sign=[^&\s\"']+", "sign=<sign>", text)


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


def af(*args):
    """alipay-fake.py's JSON line."""
    _, out = run([AF, *[str(a) for a in args]], env={"ALIPAY_DIR": f"{L}/alipay"})
    try:
        return json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"error": out.strip()[-300:]}


def imbo(name, *args):
    return run([ENV["IMBO"], *args], env={"IMBO_HOME": f"{L}/{name}"}, timeout=90)


def log():
    return open(f"{L}/server.log").read()


def events(name):
    return [ln for ln in log().splitlines() if f'event="{name}"' in ln]


def pgurl():
    port = open(f"{L}/server/pg/pgdata/postmaster.pid").read().splitlines()[3].strip()
    return f"postgres://postgres@127.0.0.1:{port}/postgres"


def sql(q):
    with psycopg.connect(pgurl(), autocommit=True) as c:
        cur = c.execute(q)
        return cur.fetchall() if cur.description else []


def packs(account):
    return sql(f"SELECT id, provider, mode, plan, status, paid_until, revoked_at FROM subscriptions "
               f"WHERE account_id = {account} AND provider = 'alipay' ORDER BY id")


def near(t, want, slack=dt.timedelta(hours=1)):
    if isinstance(t, str):
        t = dt.datetime.fromisoformat(t.replace("Z", "+00:00"))
    return abs(t - want) < slack


def billing(name, web=False):
    if web:
        return call("GET", "/v1/billing", cookie=JAR[name])
    return call("GET", "/v1/billing", token=APP[name])


def buy(name, plan="monthly", app=False):
    if app:
        return call("POST", "/v1/billing/alipay", token=APP[name], body={"plan": plan})
    return call("POST", "/v1/billing/alipay", cookie=JAR[name], origin=WEB, body={"plan": plan})


def order_no(res):
    """The order number in a purchase URL or order string."""
    b = res[1] if isinstance(res[1], dict) else {}
    signed = b.get("purchase_url") or b.get("order_string") or ""
    m = re.search(r"out_trade_no%22%3A%22(ALIPAY_[0-9_]+)", signed)
    return m.group(1) if m else None


def signed_of(res):
    b = res[1] if isinstance(res[1], dict) else {}
    return b.get("purchase_url") or b.get("order_string") or ""


def check_order(name, out_trade_no, app=False):
    body = {"out_trade_no": out_trade_no}
    if app:
        return call("POST", "/v1/billing/alipay/check", token=APP[name], body=body)
    return call("POST", "/v1/billing/alipay/check", cookie=JAR[name], origin=WEB, body=body)


def notify_lines(out_trade_no):
    return [ln for ln in events("billing_event") if out_trade_no in ln and "alipay notification" in ln]


def billing_events(out_trade_no):
    return sql(f"SELECT id, type FROM billing_events WHERE subscription_id = '{out_trade_no}' "
               f"ORDER BY received_at")


def beijing(text):
    return dt.datetime.strptime(text, "%Y-%m-%d %H:%M:%S").replace(
        tzinfo=dt.timezone(dt.timedelta(hours=8)))


def add_months(t, n):
    """Calendar months in Beijing time, the day clamped to the month's last."""
    t = t.astimezone(dt.timezone(dt.timedelta(hours=8)))
    y, m = divmod(t.month - 1 + n, 12)
    y, m = t.year + y, m + 1
    last = [31, 29 if y % 4 == 0 and (y % 100 or y % 400 == 0) else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1]
    return t.replace(year=y, month=m, day=min(t.day, last))


def checkout(name, plan="monthly"):
    return call("POST", "/v1/billing/checkout", cookie=JAR[name], origin=WEB, body={"plan": plan})


def app_login(name, platform):
    _, out = run([os.path.join(HERE, "app-login.sh"), f"{name}@imbo.test", "", platform])
    token = json.loads(out.splitlines()[0])["token"]
    SECRETS.append(token)
    APP[name] = token


def web_login(name):
    email = f"{name}@imbo.test"
    jar = f"{L}/{name}.jar"
    run([os.path.join(HERE, "web-login.sh"), email, jar])
    cookie = None
    for line in open(jar):
        parts = line.rstrip("\n").split("\t")
        if len(parts) == 7:
            cookie = f"{parts[5]}={parts[6]}"
    SECRETS.append(cookie)
    JAR[name] = cookie


def server(extra=None, drop=()):
    """`local-env.sh server` with this env, plus `extra`, without `drop`."""
    env = {k: v for k, v in ENV.items() if k not in drop}
    env.update(extra or {})
    subprocess.run([LOCAL_ENV, "server"], env=env, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=120, check=True)


def setup(contacts="alice:bob,alice:carol,bob:carol"):
    """The case's 前置: down, billing-env.sh, alipay-env.sh, up, logins."""
    global ENV
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    envfile = f"{L}.alipay-env"
    script = f"""set -e
. "{LOCAL_ENV}"
. "{HERE}/billing-env.sh"
. "{HERE}/alipay-env.sh"
"{LOCAL_ENV}" up {contacts}
env -0 > "{envfile}"
"""
    with open(f"{L}.alipay-setup.log", "w") as out:
        subprocess.run(["bash", "-c", script], env=BASE, stdout=out, stderr=out, timeout=300, check=True)
    raw = open(envfile, "rb").read().decode()
    os.remove(envfile)
    ENV = dict(kv.split("=", 1) for kv in raw.split("\0") if "=" in kv)
    SECRETS.append(ENV.get("IMBO_WAFFO_PRIVATE_KEY"))
    SECRETS.append(ENV.get("IMBO_ALIPAY_PRIVATE_KEY"))
    ID.clear()
    for line in open(f"{L}/ids.tsv"):
        parts = line.split()
        if len(parts) >= 2:
            ID[parts[0]] = int(parts[1])
    APP.clear()
    JAR.clear()
    for n in ("alice", "bob", "carol"):
        app_login(n, "ios" if n == "carol" else "android")
        web_login(n)
    say(f"   setup: up {contacts}; alice {ID['alice']}, bob {ID['bob']}, carol {ID['carol']}")


def age(account, days=31):
    run([ENV["S"], "dev", "age-account", str(account), "--days", str(days)],
        env={"IMBO_DATA_DIR": f"{L}/server"})


def gift(account, until="2036-11-01 23:59:59+08"):
    sql(f"INSERT INTO subscriptions (id, provider, mode, account_id, plan, status, paid_until, "
        f"provider_updated_at) VALUES ('gift-{account}', 'gift', 'test', {account}, 'yearly', "
        f"'scheduled_cancel', '{until}', now())")


def revoke(pack_id):
    return run([ENV["S"], "billing", "revoke", pack_id],
               env={"DATABASE_URL": pgurl(), "IMBO_MAIL_LOG_ONLY": "1"})


# ---------- TC-516 ----------

def tc516():
    tc = 516
    setup()
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    age(a, 31)
    age(b, 20)

    r = buy("alice")
    o = af("open", signed_of(r))
    no = o.get("out_trade_no") or ""
    check(tc, 1, r[0] == 200 and set((r[1] or {}).keys()) == {"purchase_url"}
          and signed_of(r).startswith("http://127.0.0.1:18093/gateway.do?")
          and o.get("sign_ok") is True and o.get("method") == "alipay.trade.page.pay"
          and o.get("app_id") == "9021000000000001"
          and re.fullmatch(rf"ALIPAY_{a}_\d{{13}}", no) is not None
          and o.get("total_amount") == "15.00" and o.get("product_code") == "FAST_INSTANT_TRADE_PAY"
          and o.get("timeout_express") == "1h"
          and o.get("return_url") == "https://app.imbo.dev/account?paid=alipay",
          "web order: a signed computer-cashier URL for 15.00 with the return address",
          (r[0], sorted((r[1] or {}).keys()), o))

    app, web = billing("alice")[1] or {}, billing("alice", web=True)[1] or {}
    check(tc, 2, app.get("alipay") is True and web.get("alipay") is True,
          "both the App and the web app may sell", (app.get("alipay"), web.get("alipay")))

    r = check_order("alice", no)
    rows = packs(a)
    _, out = imbo("alice", "send", str(b), "T516-1")
    check(tc, 3, r == (200, {"status": "unpaid"}) and rows == [] and "Subscription required" in out,
          "unpaid: check says unpaid, nothing recorded, alice can't send", (brief(r), rows, out.strip()[-100:]))

    paid = af("pay", no)
    note = paid.get("notified") or {}
    rows = packs(a)
    evs = billing_events(no)
    state = json.load(open(f"{L}/alipay/state.json"))
    paid_at = beijing(state["orders"][no]["paid_at"])
    check(tc, 4, note.get("status") == 200 and note.get("body") == "success"
          and len(rows) == 1 and rows[0][0] == no and rows[0][1:5] == ("alipay", "test", "monthly", "scheduled_cancel")
          and rows[0][5] == add_months(paid_at, 1)
          and evs == [(f"alipay:{note.get('notify_id')}", "alipay.TRADE_SUCCESS")],
          "paid: the notification records one monthly pack, a calendar month from payment",
          (note, rows, evs, paid_at.isoformat()))

    _, out = imbo("alice", "send", str(b), "T516-2")
    _, who = imbo("alice", "whoami")
    rec = events("alipay_pack_recorded")
    applied = [ln for ln in notify_lines(no) if 'outcome="applied"' in ln]
    check(tc, 5, "Sent" in out and "subscribed, paid through" in who and len(rec) == 1 and len(applied) == 1,
          "alice sends again; whoami subscribed; one alipay_pack_recorded, one applied notification",
          (out.strip()[-60:], who.strip().splitlines()[-1:], len(rec), len(applied)))

    again = af("notify", no, "--notify-id", note.get("notify_id"))
    fin = af("notify", no, "--status", "TRADE_FINISHED")
    rows2 = packs(a)
    evs = billing_events(no)
    check(tc, 6, again.get("body") == "success" and fin.get("body") == "success"
          and len(rows2) == 1 and rows2[0][5] == rows[0][5]
          and [e[1] for e in evs] == ["alipay.TRADE_SUCCESS", "alipay.TRADE_FINISHED"],
          "a resent and a later notification record nothing more (INV-033)",
          (again, fin, rows2, evs))

    r = check_order("alice", no)
    got = (r[1] or {}).get("paid_until") if isinstance(r[1], dict) else None
    check(tc, 7, r[0] == 200 and (r[1] or {}).get("status") == "granted" and got
          and dt.datetime.fromisoformat(got.replace("Z", "+00:00")) == rows[0][5],
          "check after the notification: granted with the recorded date", brief(r))

    r1, r2 = buy("alice"), checkout("alice")
    s = (billing("alice")[1] or {}).get("subscription") or {}
    check(tc, 8, problem(r1, 409, "already-subscribed") and problem(r2, 409, "already-subscribed")
          and s.get("provider") == "alipay" and s.get("status") == "scheduled_cancel",
          "a live pack blocks buying again and a Waffo checkout", (brief(r1), brief(r2), s))

    r = buy("bob", "yearly")
    o = af("open", signed_of(r))
    bno = o.get("out_trade_no") or ""
    af("pay", bno, "--quiet")
    trial_end = sql(f"SELECT greatest(created_at, timestamptz '2026-09-01 00:00:00+00') + interval '30 days' "
                    f"FROM accounts WHERE id = {b}")[0][0]
    rc = check_order("bob", bno)
    brow = packs(b)
    got = (rc[1] or {}).get("paid_until") if isinstance(rc[1], dict) else None
    check(tc, 9, o.get("total_amount") == "150.00" and rc[0] == 200 and (rc[1] or {}).get("status") == "granted"
          and got and dt.datetime.fromisoformat(got.replace("Z", "+00:00")) == add_months(trial_end, 12)
          and len(brow) == 1 and brow[0][3] == "yearly",
          "back before the notification: the check records the yearly pack from the trial's end",
          (o.get("total_amount"), brief(rc), brow, trial_end.isoformat()))

    late = af("notify", bno)
    brow2 = packs(b)
    dup = [ln for ln in notify_lines(bno) if 'outcome="duplicate"' in ln]
    check(tc, 10, late.get("body") == "success" and brow2 == brow and len(dup) == 1,
          "the late notification records nothing more", (late, brow2, len(dup)))

    sql(f"UPDATE subscriptions SET paid_until = now() - interval '1 day' WHERE account_id = {a} "
        f"AND provider = 'alipay'")
    _, out = imbo("alice", "send", str(b), "T516-3")
    check(tc, 11, "Subscription required" in out, "an expired pack grants nothing, with no 3-day grace",
          out.strip()[-120:])

    age(b, 31)
    code, out = revoke(bno)
    _, send = imbo("bob", "send", str(c), "T516-4")
    more = af("notify", bno)
    rv = sql(f"SELECT revoked_at FROM subscriptions WHERE id = '{bno}'")
    check(tc, 12, code == 0 and "revoked" in out and "Waffo" not in out and "Subscription required" in send
          and more.get("body") == "success" and len(rv) == 1 and rv[0][0] is not None,
          "revoke: no Waffo call, bob can't send, a later notification doesn't restore it",
          (out.strip()[-160:], send.strip()[-80:], more, rv))

    gift(c)
    r = buy("carol")
    o = af("open", signed_of(r))
    af("pay", o.get("out_trade_no") or "")
    s = (billing("carol")[1] or {}).get("subscription") or {}
    check(tc, 13, r[0] == 200 and s.get("provider") == "alipay" and len(packs(c)) == 1,
          "a gift doesn't block buying; the account page shows the paid pack first", (brief(r), s, packs(c)))


# ---------- TC-517 ----------

def tc517():
    tc = 517
    setup("alice:bob,alice:carol")
    a, b = ID["alice"], ID["bob"]
    age(a, 31)
    age(b, 31)

    o = af("open", signed_of(buy("alice")))
    no = o.get("out_trade_no") or ""
    forged = af("notify", no, "--forge")
    check(tc, 1, forged.get("status") == 401 and '"/errors/unauthorized"' in forged.get("body", "")
          and billing_events(no) == [],
          "a notification not signed by Alipay: 401, nothing stored", (forged, billing_events(no)))

    replies = [af("notify", no, "--app", "2021000000000009"),
               af("notify", no, "--status", "WAIT_BUYER_PAY"),
               af("notify", no, "--amount", "0.01")]
    replies.append(af("notify", "ALIPAY_99999999_1", "--app", "9021000000000001"))
    outcomes = [re.search(r'outcome="?(\w+)', ln).group(1) for ln in notify_lines(no)]
    stray = [re.search(r'outcome="?(\w+)', ln).group(1) for ln in notify_lines("ALIPAY_99999999_1")]
    check(tc, 2, all(x.get("status") == 200 and x.get("body") == "success" for x in replies)
          and len(billing_events(no)) == 3 and len(billing_events("ALIPAY_99999999_1")) == 1
          and packs(a) == [] and sql("SELECT id FROM subscriptions WHERE id = 'ALIPAY_99999999_1'") == []
          and outcomes == ["ignored", "ignored", "unmapped"] and stray == ["unmapped"],
          "another app, an unpaid status, a wrong amount, an unknown account: stored, not recorded",
          ([x.get("status") for x in replies], outcomes, stray, packs(a)))

    results = []
    for how in ("500", "error", "slow"):
        af("fail", how)
        t0 = time.monotonic()
        r = check_order("alice", no)
        results.append((how, brief(r), round(time.monotonic() - t0, 1)))
    check(tc, 3, all(problem((s, {"type": t}), 502, "payment-provider-failed") for _, (s, t, *_), _ in results)
          and results[2][2] < 7,
          "Alipay failing, erring or slower than 5 s: 502", results)

    af("fail", "off")
    r = check_order("alice", no)
    check(tc, 4, r == (200, {"status": "unpaid"}) and packs(a) == [],
          "Alipay back: unpaid, still nothing recorded", (brief(r), packs(a)))

    bo = af("open", signed_of(buy("bob")))
    rs = [check_order("alice", x) for x in (bo.get("out_trade_no") or "", f"ALIPAY_{a}", "hello")]
    check(tc, 5, all(problem(x, 404, "not-found") for x in rs),
          "someone else's or a malformed order number: 404", [brief(x) for x in rs])

    r = buy("alice", "weekly")
    check(tc, 6, problem(r, 400, "invalid-request"), "a plan other than monthly or yearly: 400", brief(r))

    r = buy("bob", "yearly", app=True)
    o = af("open", signed_of(r))
    check(tc, 7, r[0] == 200 and set((r[1] or {}).keys()) == {"order_string"}
          and not signed_of(r).startswith("http") and o.get("sign_ok") is True
          and o.get("method") == "alipay.trade.app.pay" and o.get("app_id") == "9021000000000002"
          and o.get("product_code") == "QUICK_MSECURITY_PAY" and o.get("total_amount") == "150.00"
          and o.get("return_url") is None,
          "App order: a signed order string from the mobile app, no return address",
          (r[0], sorted((r[1] or {}).keys()), o))

    bno = o.get("out_trade_no") or ""
    paid = af("pay", bno)
    rc = check_order("bob", bno, app=True)
    check(tc, 8, (paid.get("notified") or {}).get("body") == "success" and rc[0] == 200
          and (rc[1] or {}).get("status") == "granted" and len(packs(b)) == 1 and packs(b)[0][3] == "yearly",
          "the App's order is recorded and its check says granted", (paid, brief(rc), packs(b)))

    server(drop=("IMBO_ALIPAY_MOBILE_APP_ID",))
    app, r, web = billing("alice")[1] or {}, buy("alice", app=True), billing("alice", web=True)[1] or {}
    check(tc, 9, app.get("alipay") is False and problem(r, 404, "not-found") and web.get("alipay") is True,
          "no mobile app: the App doesn't sell, the web app still does",
          (app.get("alipay"), brief(r), web.get("alipay")))

    server(drop=("IMBO_ALIPAY_MOBILE_APP_ID", "IMBO_ALIPAY_APP_ID", "IMBO_ALIPAY_PRIVATE_KEY",
                 "IMBO_ALIPAY_PUBLIC_KEY", "IMBO_ALIPAY_API_URL"))
    web, r, n = billing("alice", web=True)[1] or {}, buy("alice"), af("notify", no)
    check(tc, 10, web.get("alipay") is False and problem(r, 404, "not-found") and n.get("status") == 404,
          "no Alipay: nothing sold, notifications 404", (web.get("alipay"), brief(r), n.get("status")))

    server(extra={"IMBO_DOMAIN": "api.imbo.test", "IMBO_TLS": "off"})
    o = af("open", signed_of(buy("alice")))
    check(tc, 11, o.get("sign_ok") is True
          and o.get("notify_url") == "https://api.imbo.test/v1/billing/alipay/notify",
          "with a domain, orders carry the notification address", o)
    server()

    app, r = billing("carol")[1] or {}, buy("carol", app=True)
    rc = check_order("carol", f"ALIPAY_{ID['carol']}_1", app=True)
    check(tc, 12, app.get("alipay") is False and problem(r, 404, "not-found") and problem(rc, 404, "not-found"),
          "the iPhone App: no Alipay, ordering and checking 404", (app.get("alipay"), brief(r), brief(rc)))


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
    for n, case in [("516", tc516), ("517", tc517)]:
        if only and n not in only:
            continue
        try:
            case()
        except Exception:
            check(n, "-", False, "crashed", traceback.format_exc()[-900:])
        write(run_id, n, header)
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
