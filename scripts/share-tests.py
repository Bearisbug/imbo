#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["psycopg[binary]>=3.2"]
# ///
"""TC-487 to TC-494 (docs/TEST.md): which of an account's sessions each
contact sees (FLOW-CONTACT-SHARE, RULE-018, RULE-019, API-081, API-082,
migration 0014), against the local server.

    . crates/scripts/local-env.sh
    crates/scripts/share-tests.py <RUN-ID> [487 … 494]

Run it directly so uv supplies psycopg. Each case starts its own server and
seed the way parity-tests.py does and writes its lines to
docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if set, on the first
line); exits 1 when any step failed. About two minutes.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys
import time
import uuid

import psycopg

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
spec = importlib.util.spec_from_file_location("events", os.path.join(HERE, "events-tests.py"))
E = importlib.util.module_from_spec(spec)
spec.loader.exec_module(E)
L, M, OUT, call, check, imbo, typ = P.L, os.environ["M"], P.OUT, P.call, P.check, P.imbo, P.typ
SID = "66666666-aaaa-bbbb-cccc-000000"
ENV = os.path.join(HERE, "local-env.sh")
MIG13 = os.path.join(HERE, "..", "imbo-server", "migrations", "0013_events.sql")


def fake(who, sid, title, *more):
    return imbo(who, "dev", "fake-session", f"{SID}{sid}", "--title", title, *more)


def mcp(who, tool, args, sid):
    r = subprocess.run(["python3", M, f"{L}/{who}", tool, json.dumps(args), "--session", f"claude-code:{SID}{sid}"],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


def tok(who):
    return P.computer_token(f"{L}/{who}")


def sessions_of(who, peer, want=None, offline=False):
    """Short IDs `who` sees under contact `peer` (API-012), asked again for
    up to 3 s until they equal `want`: a session reaches the server through
    its daemon a moment after `dev fake-session` returns."""
    q = "?offline=true" if offline else ""
    for _ in range(15):
        _, b, _ = call("GET", f"/v1/contacts{q}", bearer=tok(who))
        c = next((c for c in b["contacts"] if c["name"] == peer), None)
        got = None if c is None else sorted(s["short_id"] for s in c["sessions"])
        if want is None or got == want:
            return got
        time.sleep(0.2)
    return got


def send(who, to, body):
    s, b, _ = call("POST", "/v1/messages", {"to": to, "body": body, "client_msg_id": uuid.uuid4().hex},
                   bearer=tok(who))
    return s, b or {}


def share_get(who, peer_id, bearer=None):
    return call("GET", f"/v1/contacts/{peer_id}/share", bearer=bearer or tok(who))[:2]


def share_put(who, peer_id, body, bearer=None):
    return call("PUT", f"/v1/contacts/{peer_id}/share", body, bearer=bearer or tok(who))[:2]


def listed(view):
    """A share view as (share, device names, session short IDs)."""
    return (view.get("share"), [d["name"] for d in view.get("devices", [])],
            sorted(s["short_id"] for s in view.get("sessions", [])))


def not_found(s, b):
    return s == 404 and (typ(b) or "").endswith("session-not-found")


def cands(b):
    return sorted(c["to"] for c in b.get("candidates") or [])


def pgurl():
    port = open(f"{L}/server/pg/pgdata/postmaster.pid").read().splitlines()[3].strip()
    return f"postgres://postgres@127.0.0.1:{port}/postgres"


def sql(q, *args):
    with psycopg.connect(pgurl(), autocommit=True) as c:
        cur = c.execute(q, args)
        return cur.fetchall() if cur.description else None


def befriend(a, b):
    """`a` asks, `b` accepts: a contact made after migration 0014."""
    i = P.ids()
    imbo(a, "contact", "add", str(i[b]))
    _, rq, _ = call("GET", "/v1/requests?direction=in&status=pending", bearer=tok(b))
    rid = next(r["id"] for r in rq["requests"] if r["from"]["id"] == i[a])
    return imbo(b, "contact", "accept", str(rid))


def tc487():
    P.up("alice:bob")
    i = P.ids()
    fake("bob", "487001", "t487a")
    fake("bob", "487002", "t487b")
    rc, out = imbo("bob", "contact", "share", "alice")
    check(487, 1, "bob 看 alice 能看到哪些", rc == 0 and "sees all your sessions" in out, out)
    got = sessions_of("alice", "bob", ["487001", "487002"])
    check(487, 2, "alice 列联系人", got == ["487001", "487002"], f"bob 的会话={got}")
    rc, out = imbo("bob", "contact", "share", "alice", "487001")
    check(487, 3, "bob 只选 487001", rc == 0 and "Saved" in out and "487001" in out and "487002" not in out, out)
    rc, out = imbo("alice", "contact", "list")
    check(487, 4, "alice 终端列联系人", "487001" in out and "487002" not in out, out)
    got = sessions_of("alice", "bob")
    check(487, 5, "alice 接口列联系人", got == ["487001"], f"bob 的会话={got}")
    s6, b6 = send("alice", "bob/487002", "hi")
    check(487, 6, "发给没开放的会话", not_found(s6, b6) and cands(b6) == [f"{i['bob']}/487001"],
          f"status={s6} type={typ(b6)} candidates={cands(b6)}")
    s7, b7 = send("alice", "bob/zzzzzz", "hi")
    same = (s7, typ(b7), cands(b7), b7.get("detail", "").replace("zzzzzz", "X")) == \
           (s6, typ(b6), cands(b6), b6.get("detail", "").replace("487002", "X"))
    check(487, 7, "发给不存在的会话", same, f"status={s7} type={typ(b7)} candidates={cands(b7)} detail={b7.get('detail')}")
    s8, b8 = send("alice", "bob/t487b", "hi")
    check(487, 8, "按标题写没开放的会话", not_found(s8, b8) and cands(b8) == cands(b6),
          f"status={s8} type={typ(b8)} candidates={cands(b8)}")
    s9, b9 = send("alice", "bob/487001", "hi")
    check(487, 9, "发给开放的会话", s9 == 201 and b9.get("recipients") == 1, f"status={s9} body={b9}")
    rc, out = imbo("bob", "contact", "share", "alice", "none")
    check(487, 10, "bob 选一个都看不到", rc == 0 and "sees none of your sessions" in out, out)
    got = sessions_of("alice", "bob")
    s11, b11 = send("alice", "bob", "hi2")
    check(487, 11, "alice 列联系人并发收件箱", got == [] and s11 == 201 and b11.get("terminal") is True,
          f"bob 的会话={got} status={s11} terminal={b11.get('terminal')}")
    rc, out = imbo("bob", "contact", "share", "alice", "all")
    got = sessions_of("alice", "bob")
    check(487, 12, "bob 改回全部", rc == 0 and got == ["487001", "487002"], f"{out} / bob 的会话={got}")


def tc488():
    P.up("alice:bob")
    i = P.ids()
    fake("bob", "488001", "t488a")
    _, b, _ = call("GET", "/v1/devices", bearer=tok("bob"))
    names = [d["name"] for d in b["devices"]]
    check(488, 1, "bob 的电脑", len(names) == 1, f"devices={names}")
    rc, out = imbo("bob", "contact", "share", "alice", names[0])
    check(488, 2, "bob 选这台电脑", rc == 0 and "Saved" in out and "new ones too" in out, out)
    got = sessions_of("alice", "bob", ["488001"])
    check(488, 3, "alice 列联系人", got == ["488001"], f"bob 的会话={got}")
    fake("bob", "488002", "t488b")
    got = sessions_of("alice", "bob", ["488001", "488002"])
    check(488, 4, "bob 新开一个会话", got == ["488001", "488002"], f"bob 的会话={got}")
    s, v = share_get("bob", i["alice"])
    check(488, 5, "bob 读设置", s == 200 and listed(v) == ("chosen", names, []), f"status={s} view={listed(v)}")
    fake("bob", "488002", "t488b", "--remove")
    got, off = sessions_of("alice", "bob", ["488001"]), sessions_of("alice", "bob", offline=True)
    check(488, 6, "新会话下线", got == ["488001"] and off == ["488001", "488002"],
          f"在线={got} 含 7 天内={off}")


def tc489():
    P.up("alice:bob")
    fake("bob", "489001", "t489a")
    rc, out = befriend("carol", "bob")
    check(489, 1, "carol 加 bob、bob 同意", rc == 0 and "is a contact now" in out, out)
    rc, out = imbo("carol", "contact", "list")
    got = sessions_of("carol", "bob")
    check(489, 2, "carol 列联系人", got == [] and "no sessions" in out, f"bob 的会话={got}")
    rc, out = imbo("bob", "contact", "share", "carol")
    check(489, 3, "bob 看 carol 能看到哪些", "none are chosen yet" in out, out)
    s, b = send("carol", "bob/489001", "hi")
    check(489, 4, "carol 发给 bob 的会话", not_found(s, b) and cands(b) == [],
          f"status={s} type={typ(b)} candidates={cands(b)}")
    s, b = send("carol", "bob", "hi")
    check(489, 5, "carol 发进 bob 的收件箱", s == 201 and b.get("terminal") is True, f"status={s} body={b}")
    got = sessions_of("alice", "bob", ["489001"])
    check(489, 6, "老联系人 alice 列联系人", got == ["489001"], f"bob 的会话={got}")


def tc490():
    P.up("alice:bob,alice:carol")
    i = P.ids()
    befriend("carol", "bob")
    fake("bob", "490001", "t490a")
    fake("bob", "490002", "t490b")
    fake("carol", "490101", "t490c")
    out = mcp("bob", "send", {"to": "carol", "body": "hi from 490001"}, "490001")
    check(490, 1, "bob 的会话 490001 发私聊", out.startswith("Sent"), out.strip()[:200])
    got = sessions_of("carol", "bob", offline=True)
    check(490, 2, "carol 列联系人", got == ["490001"], f"bob 的会话={got}")
    s, v = share_get("bob", i["carol"])
    check(490, 3, "bob 读设置", listed(v) == ("chosen", [], ["490001"]), f"view={listed(v)}")
    out = mcp("carol", "send", {"to": f"{i['bob']}/490001", "body": "reply"}, "490101")
    check(490, 4, "carol 的会话回复 490001", out.startswith("Sent"), out.strip()[:200])
    imbo("bob", "send", "carol", "hello in person")
    s, v = share_get("bob", i["carol"])
    check(490, 5, "bob 亲手发", listed(v) == ("chosen", [], ["490001"]), f"view={listed(v)}")
    imbo("bob", "contact", "share", "carol", "none")
    mcp("bob", "send", {"to": "carol", "body": "hi from 490002"}, "490002")
    s, v = share_get("bob", i["carol"])
    check(490, 6, "一个都看不到时 490002 发私聊", listed(v) == ("chosen", [], ["490002"]), f"view={listed(v)}")
    got = sessions_of("carol", "bob", offline=True)
    check(490, 7, "carol 列联系人", got == ["490002"], f"bob 的会话={got}")
    g = P.make_group("t490g", [("bob", i["bob"]), ("carol", i["carol"])])
    imbo("bob", "group", "sessions", "t490g", "--sessions", "490001")
    out = mcp("bob", "send", {"to": f"#{g}", "body": "group hi"}, "490001")
    s, v = share_get("bob", i["carol"])
    check(490, 8, "490001 群发", out.startswith("Sent to group t490g") and listed(v) == ("chosen", [], ["490002"]), f"view={listed(v)} send={out.strip()[:120]}")
    _, b, _ = call("GET", "/v1/groups", bearer=tok("carol"))
    grp = next((x for x in b["groups"] if x["id"] == g), {})
    ses = {m["name"]: sorted(x["short_id"] for x in m.get("sessions", [])) for m in grp.get("members", [])}
    got = sessions_of("carol", "bob", offline=True)
    check(490, 9, "carol 列群", ses.get("bob") == ["490001"] and got == ["490002"],
          f"群里 bob 的会话={ses.get('bob')} 联系人下 bob 的会话={got}")


def tc491():
    P.up("alice:bob")
    i = P.ids()
    fake("bob", "491001", "t491a")
    inside = {"CLAUDE_CODE_SESSION_ID": f"{SID}491001"}
    rc, out = imbo("bob", "contact", "share", "alice", "none", env=inside)
    check(491, 1, "会话里改", rc != 0 and "outside any agent session" in out, out)
    rc, out = imbo("bob", "contact", "share", "alice", env=inside)
    check(491, 2, "会话里看", rc == 0 and "sees all your sessions" in out, out)
    s, v = share_get("bob", i["alice"])
    check(491, 3, "设置没变", listed(v)[0] == "all", f"view={listed(v)}")


def tc492():
    P.up("alice:bob,alice:carol")
    i = P.ids()
    fake("bob", "492001", "t492a")
    fake("carol", "492101", "t492c")
    _, mine = call("GET", "/v1/agents", bearer=tok("bob"))[:2]
    _, theirs = call("GET", "/v1/agents", bearer=tok("carol"))[:2]
    bob_s = next(a["id"] for a in mine["agents"] if a["short_id"] == "492001")
    carol_s = next(a["id"] for a in theirs["agents"] if a["short_id"] == "492101")
    carol_d = next(a["device_id"] for a in theirs["agents"] if a["short_id"] == "492101")
    s, b = share_put("bob", i["alice"], {"share": "chosen", "sessions": [carol_s]})
    check(492, 1, "名单里有别人的会话", s == 400 and (typ(b) or "").endswith("invalid-request"), f"status={s} type={typ(b)}")
    s, b = share_put("bob", i["alice"], {"share": "chosen", "devices": [carol_d]})
    check(492, 2, "名单里有别人的电脑", s == 400 and (typ(b) or "").endswith("invalid-request"), f"status={s} type={typ(b)}")
    s, b = share_put("bob", i["carol"], {"share": "all"})
    check(492, 3, "对方不是联系人", s == 403 and (typ(b) or "").endswith("not-contact"), f"status={s} type={typ(b)}")
    s, b = share_put("bob", i["alice"], {"share": "some"})
    check(492, 4, "档位写错", s == 400 and (typ(b) or "").endswith("invalid-request"), f"status={s} type={typ(b)}")
    many = [str(uuid.uuid4()) for _ in range(201)]
    s, b = share_put("bob", i["alice"], {"share": "chosen", "sessions": many})
    check(492, 5, "名单 201 项", s == 400 and "200" in (b or {}).get("detail", ""), f"status={s} detail={(b or {}).get('detail')}")
    rc, out = imbo("bob", "contact", "share", "alice", "nosuch")
    check(492, 6, "终端写了找不到的名字", rc != 0 and "No computer or session called nosuch" in out and "492001" in out, out)
    s, v = share_get("bob", i["alice"])
    check(492, 7, "设置没变", listed(v)[0] == "all", f"view={listed(v)}")
    app = P.app_token("bob@imbo.test")
    s, v = share_put("bob", i["alice"], {"share": "chosen", "sessions": [bob_s]}, bearer=app)
    check(492, 8, "App 登录改", s == 200 and listed(v) == ("chosen", [], ["492001"]), f"status={s} view={listed(v)}")


def tc493():
    P.up("alice:bob", IMBO_WEB_ORIGIN=P.ORIGIN)
    i = P.ids()
    fake("bob", "493001", "t493a")
    fake("bob", "493002", "t493b")
    _, mine = call("GET", "/v1/agents", bearer=tok("bob"))[:2]
    a1 = next(a["id"] for a in mine["agents"] if a["short_id"] == "493001")
    bob_app, alice_app = P.app_token("bob@imbo.test"), P.app_token("alice@imbo.test")
    wa, wb = E.Conn(Authorization=f"Bearer {alice_app}"), E.Conn(Authorization=f"Bearer {bob_app}")
    check(493, 1, "alice 与 bob 连上", (wa.status, wb.status) == (101, 101), f"status={wa.status},{wb.status}")

    def step(n, what, action, want_a, want_b):
        wa.drain()
        wb.drain()
        action()
        ga, gb = wa.areas(), wb.areas()
        check(493, n, what, want_a(ga) and want_b(gb), f"alice={ga} bob={gb}")

    has = lambda x: lambda g: x in g
    lacks = lambda x: lambda g: x not in g
    step(2, "bob 只给 alice 看 493001",
         lambda: share_put("bob", i["alice"], {"share": "chosen", "sessions": [a1]}, bearer=bob_app),
         has("contacts"), has("contacts"))
    step(3, "alice 看不到的 493002 下线", lambda: fake("bob", "493002", "t493b", "--remove"),
         lacks("contacts"), has("agents"))
    step(4, "alice 看得到的 493001 下线", lambda: fake("bob", "493001", "t493a", "--remove"),
         has("contacts"), has("agents"))
    fake("bob", "493002", "t493b")
    step(5, "493002 给 alice 发私聊", lambda: mcp("bob", "send", {"to": "alice", "body": "hi"}, "493002"),
         has("contacts"), has("contacts"))


def tc494():
    P.up("")
    # The database as it was before migration 0014.
    sql("DROP TABLE contact_shares")
    sql("DROP TRIGGER contacts_share_events ON contacts")
    sql("DROP FUNCTION imbo_shares_changed()")
    sql("ALTER TABLE contacts DROP COLUMN share")
    sql(re.search(r"CREATE FUNCTION imbo_agents_changed\(\).*?END \$\$;", open(MIG13).read(), re.S).group(0)
        .replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION"))
    sql("DROP FUNCTION imbo_session_visible(integer, integer, uuid, uuid)")
    sql("DELETE FROM _sqlx_migrations WHERE version = 14")
    cols = sql("SELECT column_name FROM information_schema.columns WHERE table_name = 'contacts'")
    check(494, 1, "库回到迁移 0014 之前", "share" not in [c[0] for c in cols], f"contacts 的列={[c[0] for c in cols]}")
    sql("INSERT INTO contacts (account_id, peer_id) SELECT a.id, b.id FROM accounts a, accounts b "
        "WHERE a.name = 'alice' AND b.name = 'bob' UNION ALL SELECT b.id, a.id FROM accounts a, accounts b "
        "WHERE a.name = 'alice' AND b.name = 'bob'")
    check(494, 2, "alice 与 bob 成为联系人", sql("SELECT count(*) FROM contacts")[0][0] == 2, "contacts 两行")
    log = f"{L}/bob/daemon.log"
    before = open(log).read().count("connected")
    subprocess.run(["sh", ENV, "server"], check=True, capture_output=True)
    # bob's daemon reconnects on its own; his sessions reach the server through it.
    for _ in range(150):
        if open(log).read().count("connected") > before:
            break
        time.sleep(0.2)
    done = sql("SELECT count(*) FROM _sqlx_migrations WHERE version = 14")[0][0]
    check(494, 3, "重启服务端", done == 1, f"迁移 14 的记录={done}")
    rows = sql("SELECT share FROM contacts")
    check(494, 4, "已有的两行", [r[0] for r in rows] == ["all", "all"], f"share={rows}")
    rc, out = befriend("carol", "bob")
    rows = sql("SELECT share FROM contacts c JOIN accounts a ON a.id = c.account_id WHERE a.name IN ('bob', 'carol') "
               "AND c.peer_id IN (SELECT id FROM accounts WHERE name IN ('bob', 'carol'))")
    check(494, 5, "carol 与 bob 新成为联系人", [r[0] for r in rows] == ["chosen", "chosen"], f"share={rows} {out}")
    fake("bob", "494001", "t494a")
    got = sessions_of("alice", "bob", ["494001"])
    check(494, 6, "alice 列联系人", got == ["494001"], f"bob 的会话={got}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or [str(n) for n in range(487, 495)]
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
