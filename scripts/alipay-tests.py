#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["psycopg[binary]>=3.2"]
# ///
"""TC-484 (docs/TEST.md, 支付宝时长包) against the local server, with real
HTTP requests, the fake Watcha of watcha-fake.py and real database queries.

    . crates/scripts/local-env.sh
    crates/scripts/alipay-tests.py <RUN-ID> [TC numbers…]

The case builds its own environment as its 前置 says: `local-env.sh down`,
then billing-env.sh, watcha-env.sh, `local-env.sh up alice:bob` and App
tokens. Prints PASS or FAIL for every step and writes the lines to
docs/test-runs/<run>-tc-NNN.txt (EVIDENCE_DIR overrides the directory;
RUN_HEADER goes into the first line); exits 1 when any step failed. Leaves
the environment up; `local-env.sh down` ends it.

Tokens, cookies, codes and purchase tickets are never written out.
"""

import datetime as dt
import json
import os
import re
import subprocess
import sys
import traceback
import urllib.error
import urllib.request

import psycopg

HERE = os.path.dirname(os.path.abspath(__file__))
L = os.environ["L"]
LOCAL_ENV = os.path.join(HERE, "local-env.sh")
WF = os.path.join(HERE, "watcha-fake.py")
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
    # A purchase link carries a one-time ticket (API-080).
    return re.sub(r"ticket=[^&\s\"']+", "ticket=<ticket>", text)


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


def wf(*args):
    return run([WF, *[str(a) for a in args]], env={"WATCHA_DIR": f"{L}/watcha"})


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


def billing(name):
    return call("GET", "/v1/billing", token=APP[name])


def buy(name):
    return call("POST", "/v1/billing/alipay", cookie=JAR[name], origin=WEB)


def checkout(name, plan="monthly"):
    return call("POST", "/v1/billing/checkout", cookie=JAR[name], origin=WEB, body={"plan": plan})


def app_login(name):
    _, out = run([os.path.join(HERE, "app-login.sh"), f"{name}@imbo.test"])
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
    """The case's 前置: down, billing-env.sh, watcha-env.sh, up, logins."""
    global ENV
    subprocess.run([LOCAL_ENV, "down"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    envfile = f"{L}.alipay-env"
    script = f"""set -e
. "{LOCAL_ENV}"
. "{HERE}/billing-env.sh"
. "{HERE}/watcha-env.sh"
"{LOCAL_ENV}" up {contacts}
env -0 > "{envfile}"
"""
    with open(f"{L}.alipay-setup.log", "w") as out:
        subprocess.run(["bash", "-c", script], env=BASE, stdout=out, stderr=out, timeout=300, check=True)
    raw = open(envfile, "rb").read().decode()
    os.remove(envfile)
    ENV = dict(kv.split("=", 1) for kv in raw.split("\0") if "=" in kv)
    SECRETS.append(ENV.get("IMBO_WAFFO_PRIVATE_KEY"))
    ID.clear()
    for line in open(f"{L}/ids.tsv"):
        parts = line.split()
        if len(parts) >= 2:
            ID[parts[0]] = int(parts[1])
    APP.clear()
    JAR.clear()
    for n in ("alice", "bob", "carol"):
        app_login(n)
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


# ---------- TC-484 ----------

def tc484():
    tc = 484
    setup()
    a, b = ID["alice"], ID["bob"]
    age(a, 31)

    r = buy("alice")
    url = ((r[1] or {}).get("purchase_url") or "")
    bill = billing("alice")[1] or {}
    check(tc, 1, r[0] == 200 and (r[1] or {}).get("status") == "purchase_required"
          and url.startswith("http://127.0.0.1:18092/purchase?ticket=")
          and ("return_url=https%3A%2F%2Fapp.imbo.dev%2Faccount%3Fpaid%3Dalipay" in url
               or url.endswith("return_url=https://app.imbo.dev/account?paid=alipay")),
          "unpaid: purchase_required with the Watcha link and the return address",
          (r[0], (r[1] or {}).get("status"), hide(url)))
    check(tc, 2, bill.get("alipay") is True, "GET /v1/billing says the web app may sell a pack",
          {"alipay": bill.get("alipay"), "access": bill.get("access")})

    rows = packs(a)
    _, out = imbo("alice", "send", str(b), "T484-1")
    check(tc, 3, rows == [] and "Subscription required" in out,
          "nothing recorded and alice still can't send", (rows, out.strip()[-120:]))

    _, paid = wf("pay", a, "--plan", "monthly")
    r = buy("alice")
    rows = packs(a)
    starts = dt.datetime.fromisoformat(paid.split()[2])
    want = starts + dt.timedelta(days=31)
    check(tc, 4, r[0] == 200 and (r[1] or {}).get("status") == "granted"
          and near((r[1] or {}).get("paid_until") or "1970-01-01T00:00:00Z", want)
          and len(rows) == 1 and rows[0][0] == f"ALIPAY_{a}_{int(starts.timestamp())}"
          and rows[0][1:5] == ("alipay", "test", "monthly", "scheduled_cancel"),
          "paid: granted, one row, the monthly pack as 'won't renew'", (brief(r), rows))

    _, out = imbo("alice", "send", str(b), "T484-2")
    _, who = imbo("alice", "whoami")
    rec = events("alipay_pack_recorded")
    check(tc, 5, "Sent" in out and "subscribed, paid through" in who and len(rec) == 1,
          "alice sends again, whoami says subscribed, one alipay_pack_recorded",
          (out.strip()[-60:], who.strip().splitlines()[-1:], len(rec)))

    billing("alice")
    billing("alice")
    check(tc, 6, len(packs(a)) == 1, "asking Watcha again records no second row (INV-033)", packs(a))

    r1, r2 = buy("alice"), checkout("alice")
    s = (billing("alice")[1] or {}).get("subscription") or {}
    check(tc, 7, problem(r1, 409, "already-subscribed") and problem(r2, 409, "already-subscribed")
          and s.get("provider") == "alipay" and s.get("status") == "scheduled_cancel",
          "a live pack blocks both buying again and a Waffo checkout",
          (brief(r1), brief(r2), s))

    sql(f"UPDATE subscriptions SET paid_until = now() - interval '1 day' WHERE account_id = {a} "
        f"AND provider = 'alipay'")
    _, out = imbo("alice", "send", str(b), "T484-3")
    check(tc, 8, "Subscription required" in out, "an expired pack grants nothing, with no 3-day grace",
          out.strip()[-120:])

    # Buying inside the trial: bob has 10 days left, so the pack starts after them.
    age(b, 20)
    _, paid = wf("pay", b, "--plan", "yearly")
    r = buy("bob")
    starts = dt.datetime.fromisoformat(paid.split()[2])
    trial_left = dt.timedelta(days=10)
    rows = packs(b)
    check(tc, 9, r[0] == 200 and (r[1] or {}).get("status") == "granted"
          and near((r[1] or {}).get("paid_until") or "1970-01-01T00:00:00Z",
                   starts + dt.timedelta(days=365) + trial_left, dt.timedelta(hours=6))
          and len(rows) == 1 and rows[0][3] == "yearly",
          "bought inside the trial: the yearly pack runs from the trial's end",
          (brief(r), rows))

    pack_id = rows[0][0]
    # The trial has to be over too, or it would grant what the pack no
    # longer does.
    age(b, 31)
    code, out = revoke(pack_id)
    # To carol, who is still in her trial: alice stopped being able to receive
    # at step 8, and a refused recipient hides the sender's own refusal.
    _, send = imbo("bob", "send", str(ID["carol"]), "T484-4")
    after = buy("bob")
    check(tc, 10, code == 0 and "revoked" in out and "Waffo" not in out
          and "Subscription required" in send
          and after[0] == 200 and (after[1] or {}).get("status") == "granted",
          "revoke: no Waffo call, bob can't send, and asking Watcha again does not restore it",
          (out.strip()[-160:], send.strip()[-80:], brief(after)))
    check(tc, 11, len(packs(b)) == 1 and packs(b)[0][6] is not None,
          "the revoked row stays revoked", packs(b))

    sql(f"DELETE FROM subscriptions WHERE account_id = {a}")
    for how in ("500", "unknown", "unavailable"):
        wf("fail", how)
        r = buy("alice")
        if not check(tc, 12, problem(r, 502, "payment-provider-failed"),
                     f"Watcha answers {how}: 502 payment-provider-failed, nothing recorded", brief(r)):
            break
    wf("fail", "off")
    check(tc, 13, packs(a) == [], "no row from any failed answer", packs(a))

    server(drop=("IMBO_WATCHA_API_KEY", "IMBO_WATCHA_ENTITLEMENT_ID", "IMBO_WATCHA_API_URL"))
    r = buy("alice")
    bill = billing("alice")[1] or {}
    check(tc, 14, problem(r, 404, "not-found") and bill.get("alipay") is False,
          "no Watcha configured: 404 and the web app does not offer it", (brief(r), bill.get("alipay")))

    server()
    r = call("POST", "/v1/billing/alipay", token=APP["alice"])
    check(tc, 15, problem(r, 403, "forbidden"), "the App may not buy a pack", brief(r))

    # A gifted account that buys a pack still gets it recorded (ADR-060).
    c = ID["carol"]
    gift(c)
    wf("pay", c, "--plan", "monthly")
    s = (billing("carol")[1] or {}).get("subscription") or {}
    check(tc, 16, s.get("provider") == "alipay" and len(packs(c)) == 1,
          "a gift does not stop the pack from being recorded", (s, packs(c)))


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
    for n, case in [("484", tc484)]:
        if only and n not in only:
            continue
        try:
            case()
        except Exception:
            check(n, "-", False, "crashed", traceback.format_exc()[-900:])
        write(run_id, n, header)
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
