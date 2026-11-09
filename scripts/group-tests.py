#!/usr/bin/env python3
"""TC-610, TC-613, TC-629, TC-631 and TC-633 (docs/TEST.md): group admins,
groups anyone can find, asking to join one and inviting back someone removed
(FLOW-GROUP-DISCOVER, FLOW-GROUP-APPROVE, FLOW-GROUP-ADMIN, FLOW-GROUP-REMOVE,
RULE-009, RULE-023, RULE-024, API-087..090), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/group-tests.py <RUN-ID> [610 613 629 631 633]

Each case starts its own server and seed the way parity-tests.py does and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed.
"""

import http.client
import importlib.util
import json
import os
import re
import struct
import subprocess
import sys
import time
import zlib
from urllib.parse import quote

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, OUT, check, imbo, typ = P.L, P.OUT, P.check, P.imbo, P.typ
ENV = os.path.join(HERE, "local-env.sh")


def api(who, method, path, body=None, session=None):
    """`who`'s computer token, or one of their sessions with `session`: (status, body)."""
    h = {"content-type": "application/json", "authorization": f"Bearer {P.computer_token(f'{L}/{who}')}"}
    if session:
        h["imbo-actor-session"] = f"claude-code:{session}"
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request(method, path, body=json.dumps(body) if body is not None else None, headers=h)
    r = c.getresponse()
    raw = r.read()
    c.close()
    return r.status, (json.loads(raw) if raw else {})


def search(who, q):
    return api(who, "GET", f"/v1/groups/search?q={quote(q)}")


def apply(who, g, note=None):
    return api(who, "POST", f"/v1/groups/{g}/join-requests", {"note": note} if note is not None else {})


def group(who, g):
    s, b = api(who, "GET", "/v1/groups")
    return next((x for x in b.get("groups", []) if x["id"] == g), {})


def roles(gv):
    return {m["name"]: m.get("role") for m in gv.get("members", [])}


def joins(b):
    return [r for r in b.get("requests", []) if r.get("kind") == "join"]


def dave(contact):
    """dave, whom local-env.sh doesn't make, in $L/extra/dave and a contact of `contact`: his account ID."""
    r = subprocess.run([os.environ["S"], "dev", "seed", "--homes", f"{L}/extra", "--names", f"{contact},dave",
                        "--contacts", f"{contact}:dave"], check=True, capture_output=True, text=True,
                       env={**os.environ, "IMBO_DATA_DIR": f"{L}/server"})
    return next(int(l.split("\t")[1]) for l in r.stdout.splitlines() if l.startswith("dave\t"))


DAVE = "extra/dave"


def notice(who, *parts):
    """The daemon log line of a notice to `who` with all of `parts`, waiting up to 6 s."""
    for _ in range(30):
        hit = [l.strip() for l in open(f"{L}/{who}/daemon.log", errors="replace")
               if "notice:" in l and all(p in l for p in parts)]
        if hit:
            return hit[-1]
        time.sleep(0.2)
    return None


def png(w, h):
    """A plain grey w×h PNG."""
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    rows = b"".join(b"\x00" + b"\x80" * 3 * w for _ in range(h))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def picture(who, g, raw):
    """`who` puts `raw` as the group's picture (API-056): (status, body)."""
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=30)
    c.request("PUT", f"/v1/groups/{g}/avatar", body=raw,
              headers={"content-type": "image/png", "authorization": f"Bearer {P.computer_token(f'{L}/{who}')}"})
    r = c.getresponse()
    body = r.read()
    c.close()
    return r.status, (json.loads(body) if body else {})


def tc610():
    P.up("alice:bob,alice:carol")
    dave("bob")
    A, B = P.ids()["alice"], P.ids()["bob"]
    g = P.make_group("t610", [("bob", B)])

    gv = group("alice", g)
    s, b = search("carol", "t610")
    check(610, 1, "new group: not findable, owner and member", gv.get("discoverable") is False
          and roles(gv) == {"alice": "owner", "bob": "member"} and s == 200 and b.get("groups") == [],
          f"discoverable={gv.get('discoverable')} roles={roles(gv)}; search {s} {b}")

    s, b = api("bob", "PATCH", f"/v1/groups/{g}", {"discoverable": True})
    check(610, 2, "a member can't turn it on", s == 403 and typ(b) == "/errors/not-owner", f"{s} {typ(b)}")

    s, b = api("alice", "PATCH", f"/v1/groups/{g}", {"discoverable": True})
    check(610, 3, "the owner turns it on", s == 200 and b.get("group", {}).get("discoverable") is True,
          f"{s} discoverable={b.get('group', {}).get('discoverable')}")

    want = [{"id": g, "name": "t610", "member_count": 2, "owner": {"id": A, "name": "alice"},
             "joined": False, "applied": False}]
    got = [search("carol", q) for q in ("T610", f"#{g}", str(g))]
    check(610, 4, "found by name in any case, by #ID and by ID", all(s == 200 and b.get("groups") == want for s, b in got),
          "; ".join(f"{s} {b.get('groups')}" for s, b in got))

    (s1, b1), (s2, b2) = search("carol", " "), search("carol", "x" * 65)
    check(610, 5, "blank or over 64 characters: refused", s1 == 400 and typ(b1) == "/errors/invalid-request"
          and s2 == 400 and typ(b2) == "/errors/invalid-request", f"{s1} {typ(b1)}; {s2} {typ(b2)}")

    s1, b1 = apply("carol", g, "字" * 201)
    s2, b2 = apply("carol", g, "T610 想一起看日志")
    r = b2.get("request", {})
    check(610, 6, "a note over 200 is refused, then the request goes in", s1 == 400 and typ(b1) == "/errors/invalid-request"
          and s2 == 201 and r.get("kind") == "join" and r.get("group") == {"id": g, "name": "t610"}
          and "to" not in r and r.get("status") == "pending", f"{s1} {typ(b1)}; {s2} {r}")

    s, b = apply("carol", g)
    _, f = search("carol", "t610")
    check(610, 7, "asked already: refused, marked applied", s == 409 and typ(b) == "/errors/request-pending"
          and [x["applied"] for x in f.get("groups", [])] == [True], f"{s} {typ(b)}; {f.get('groups')}")

    s, b = apply("bob", g)
    _, f = search("bob", "t610")
    check(610, 8, "a member: refused, marked joined", s == 409 and typ(b) == "/errors/already-member"
          and [x["joined"] for x in f.get("groups", [])] == [True], f"{s} {typ(b)}; {f.get('groups')}")

    _, with_join = api("alice", "GET", "/v1/requests?include=join")
    _, without = api("alice", "GET", "/v1/requests")
    j = joins(with_join)
    check(610, 9, "the owner sees it with its note, only with include=join", len(j) == 1
          and j[0]["from"]["name"] == "carol" and j[0].get("note") == "T610 想一起看日志" and not joins(without),
          f"with: {j}; without: {joins(without)}")

    _, bob_in = api("bob", "GET", "/v1/requests?include=join")
    _, carol_out = api("carol", "GET", "/v1/requests?direction=out&include=join")
    j = joins(carol_out)
    check(610, 10, "a member doesn't see it; the asker sees it without the note", not joins(bob_in)
          and len(j) == 1 and "note" not in j[0], f"bob: {joins(bob_in)}; carol: {j}")

    s, _ = api("alice", "PATCH", f"/v1/groups/{g}", {"discoverable": False})
    _, f = search("carol", "t610")
    sd, bd = apply(DAVE, g)
    _, still = api("alice", "GET", "/v1/requests?include=join")
    check(610, 11, "turned off: not found, no new requests, carol's stays", s == 200 and f.get("groups") == []
          and sd == 404 and typ(bd) == "/errors/not-found" and len(joins(still)) == 1,
          f"{s}; {f.get('groups')}; dave {sd} {typ(bd)}; {len(joins(still))} join requests")

    s, b = api("alice", "PATCH", f"/v1/groups/{g}", {})
    check(610, 12, "an empty change is refused", s == 400 and typ(b) == "/errors/invalid-request", f"{s} {typ(b)}")


def tc629():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    A, B, C, D = ids["alice"], ids["bob"], ids["carol"], dave("alice")
    g = P.make_group("t629", [("bob", B), ("carol", C), (f"{L}/{DAVE}", D)])
    S = "55555555-aaaa-bbbb-cccc-000000629001"
    imbo("bob", "dev", "fake-session", S, "--title", "b629")

    s, b = api("bob", "PUT", f"/v1/groups/{g}/admins/{C}")
    check(629, 1, "a member can't make admins", s == 403 and typ(b) == "/errors/not-owner"
          and f"Only the owner and admins of t629 (#{g}) can choose its admins." in b.get("detail", ""),
          f"{s} {typ(b)} {b.get('detail')}")

    s1, _ = api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    s2, b2 = api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    r = roles(b2.get("group", {}))
    check(629, 2, "the owner makes bob an admin, twice", s1 == 200 and s2 == 200
          and r == {"alice": "owner", "bob": "admin", "carol": "member", "dave": "member"}, f"{s1} {s2} {r}")

    got = [api("alice", "PUT", f"/v1/groups/{g}/admins/{A}"), api("bob", "PUT", f"/v1/groups/{g}/admins/{A}"),
           api("bob", "PUT", f"/v1/groups/{g}/admins/99999999")]
    check(629, 3, "not the owner, by her or by the admin; not a stranger",
          [(s, typ(b)) for s, b in got] == [(400, "/errors/invalid-request")] * 2 + [(404, "/errors/not-found")],
          "; ".join(f"{s} {typ(b)}" for s, b in got))

    s1, _ = api("bob", "PUT", f"/v1/groups/{g}/admins/{C}")
    s2, b2 = api("bob", "PUT", f"/v1/groups/{g}/admins/{C}")
    r = roles(b2.get("group", {}))
    check(629, 4, "the admin makes carol an admin, twice", s1 == 200 and s2 == 200 and r.get("carol") == "admin",
          f"{s1} {s2} {r}")

    s1, _ = api("bob", "DELETE", f"/v1/groups/{g}/admins/{C}")
    s2, b2 = api("bob", "DELETE", f"/v1/groups/{g}/admins/{C}")
    r = roles(b2.get("group", {}))
    check(629, 5, "the admin unsets carol, twice", s1 == 200 and s2 == 200 and r.get("carol") == "member",
          f"{s1} {s2} {r}")

    sn, bn = api("bob", "PATCH", f"/v1/groups/{g}", {"name": "t629b"})
    sd, bd = api("bob", "PATCH", f"/v1/groups/{g}", {"discoverable": True})
    sp, bp = picture("bob", g, png(40, 40))
    sx, _ = api("bob", "DELETE", f"/v1/groups/{g}/avatar")
    sm, bm = api("carol", "PATCH", f"/v1/groups/{g}", {"name": "x"})
    check(629, 6, "the admin renames it, lets anyone find it, puts and removes its picture; a member can't rename",
          sn == 200 and bn.get("group", {}).get("name") == "t629b" and sd == 200
          and bd.get("group", {}).get("discoverable") is True and sp == 200 and bool(bp.get("avatar_url")) and sx == 204
          and sm == 403 and typ(bm) == "/errors/not-owner",
          f"{sn} {bn.get('group', {}).get('name')}; {sd} {bd.get('group', {}).get('discoverable')}; "
          f"picture {sp} {bp.get('avatar_url')}; {sx}; carol {sm} {typ(bm)}")

    s, _ = api("bob", "DELETE", f"/v1/groups/{g}/members/{C}")
    box = imbo("carol", "inbox", "--all")[1]
    check(629, 7, "the admin removes carol, the notice names bob", s == 204
          and f"You were removed from group t629b (#{g}) by bob." in box, f"{s}; {box[-300:]}")

    s0, _ = api("alice", "PUT", f"/v1/groups/{g}/admins/{D}")
    s1, _ = api("bob", "DELETE", f"/v1/groups/{g}/members/{D}")
    box = imbo(DAVE, "inbox", "--all")[1]
    s2, b2 = api("bob", "DELETE", f"/v1/groups/{g}/members/{A}")
    check(629, 8, "the admin removes the admin dave, not the owner", s0 == 200 and s1 == 204
          and f"You were removed from group t629b (#{g}) by bob." in box
          and s2 == 403 and typ(b2) == "/errors/not-owner" and f"The owner of t629b (#{g}) can't be removed." in b2.get("detail", ""),
          f"{s0}; {s1}; dave: {box[-200:]}; {s2} {typ(b2)} {b2.get('detail')}")

    got = [api("bob", "PUT", f"/v1/groups/{g}/owner", {"account_id": B}), api("bob", "DELETE", f"/v1/groups/{g}")]
    check(629, 9, "the admin can't hand it on or dissolve it",
          all(s == 403 and typ(b) == "/errors/not-owner" for s, b in got), "; ".join(f"{s} {typ(b)}" for s, b in got))

    s, b = api("alice", "PUT", f"/v1/groups/{g}/owner", {"account_id": B})
    r = roles(b.get("group", {}))
    check(629, 10, "handed to the admin: bob owns it, alice is a member, no admins", s == 200
          and r == {"alice": "member", "bob": "owner"}, f"{s} {r}")

    s, b = api("bob", "PUT", f"/v1/groups/{g}/admins/{A}", session=S)
    check(629, 11, "an agent can't make admins", s == 403 and typ(b) == "/errors/forbidden", f"{s} {typ(b)}")

    s1, _ = api("bob", "PUT", f"/v1/groups/{g}/admins/{A}")
    s2, _ = api("alice", "DELETE", f"/v1/groups/{g}/members/me")
    r = roles(group("bob", g))
    check(629, 12, "an admin leaves like anyone", s1 == 200 and s2 == 204 and "alice" not in r, f"{s1} {s2} {r}")


def tc631():
    P.up("alice:bob,alice:carol")
    D = dave("bob")
    ids = P.ids()
    B, C = ids["bob"], ids["carol"]
    g = P.make_group("t631", [("bob", B)])
    api("alice", "PATCH", f"/v1/groups/{g}", {"discoverable": True})
    api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    S = "55555555-aaaa-bbbb-cccc-000000631001"
    imbo("alice", "dev", "fake-session", S, "--title", "a631")

    s1, b1 = apply("carol", g, "T631 carol")
    s2, b2 = apply(DAVE, g, "T631 dave")
    r1, r2 = b1.get("request", {}).get("id"), b2.get("request", {}).get("id")
    check(631, 1, "carol and dave ask", s1 == 201 and s2 == 201, f"{s1} #{r1}; {s2} #{r2}")

    _, bob_in = api("bob", "GET", "/v1/requests?include=join")
    _, agent_in = api("alice", "GET", "/v1/requests?include=join", session=S)
    notes = {r["id"]: r.get("note") for r in joins(bob_in)}
    check(631, 2, "the admin sees both with notes; the owner's session sees none",
          notes == {r1: "T631 carol", r2: "T631 dave"} and not joins(agent_in), f"bob: {notes}; session: {joins(agent_in)}")

    s, b = api("alice", "POST", f"/v1/requests/{r1}/accept", session=S)
    check(631, 3, "the owner's session can't accept", s == 404 and typ(b) == "/errors/not-found", f"{s} {typ(b)}")

    s, b = api("bob", "POST", f"/v1/requests/{r1}/accept")
    gv = group("carol", g)
    check(631, 4, "the admin accepts: carol is in, no sessions picked", s == 200
          and b.get("request", {}).get("status") == "accepted" and gv.get("my_sessions") == [],
          f"{s} {b.get('request', {}).get('status')}; carol's t631 {gv.get('name')} my_sessions={gv.get('my_sessions')}")

    s, b = api("alice", "POST", f"/v1/requests/{r1}/accept")
    check(631, 5, "already handled", s == 409 and typ(b) == "/errors/request-closed", f"{s} {typ(b)}")

    s, b = api("carol", "POST", f"/v1/requests/{r2}/accept")
    check(631, 6, "a member can't accept", s == 404 and typ(b) == "/errors/not-found", f"{s} {typ(b)}")

    s, b = api("alice", "POST", f"/v1/requests/{r2}/decline")
    _, out = api(DAVE, "GET", "/v1/requests?direction=out&include=join&status=all")
    mine = {r["id"]: r["status"] for r in joins(out)}
    check(631, 7, "the owner declines; dave sees it declined", s == 200
          and b.get("request", {}).get("status") == "declined" and mine.get(r2) == "declined", f"{s}; dave {mine}")

    s, b = apply(DAVE, g)
    r3 = b.get("request", {}).get("id")
    check(631, 8, "declined, so dave can ask again", s == 201, f"{s} #{r3}")

    s1, _ = api("alice", "DELETE", f"/v1/groups/{g}/members/{C}")
    s2, b2 = apply("carol", g)
    r4 = b2.get("request", {}).get("id")
    check(631, 9, "carol removed, then asks again", s1 == 204 and s2 == 201, f"{s1}; {s2} #{r4}")

    s, b = api("bob", "POST", f"/v1/requests/{r4}/accept")
    gv = group("alice", g)
    check(631, 10, "the admin lets a removed one back", s == 200 and b.get("request", {}).get("status") == "accepted"
          and "carol" in roles(gv), f"{s} {typ(b)}; members {roles(gv)}")

    s1, _ = api("alice", "POST", f"/v1/requests/{r3}/accept")
    s2, _ = api("bob", "DELETE", f"/v1/groups/{g}/members/{D}")
    s3, b3 = api("bob", "POST", f"/v1/groups/{g}/invites", {"account_id": D})
    r5 = b3.get("request", {}).get("id")
    s4, _ = api(DAVE, "POST", f"/v1/requests/{r5}/accept")
    gv = group("alice", g)
    check(631, 11, "dave in, removed by the admin, back on the admin's invite", (s1, s2, s3, s4) == (200, 204, 201, 200)
          and "dave" in roles(gv), f"{s1} {s2} {s3} #{r5} {s4}; members {roles(gv)}")


def tc613():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    B, C = ids["bob"], ids["carol"]
    g = P.make_group("t613", [("bob", B)])

    rc, out = imbo("alice", "group", "--help")
    check(613, 1, "help lists the new subcommands", all(f"  {w} " in out for w in ("search", "apply", "admin", "discoverable")),
          out)

    rc, out = imbo("alice", "group", "discoverable", "t613", "on")
    check(613, 2, "discoverable on", rc == 0 and f"Anyone can now find t613 (#{g})" in out, out)

    rc, out = imbo("alice", "group", "admin", "t613", str(B))
    _, listed = imbo("alice", "group", "list")
    check(613, 3, "bob made an admin, marked in the list", rc == 0 and "bob is now an admin of t613" in out
          and "bob admin" in listed, f"{out} | {listed}")

    rc, out = imbo("carol", "group", "search", "t613")
    line = next((l for l in out.splitlines() if f"#{g}" in l), "")
    check(613, 4, "carol finds it", all(p in line for p in ("t613", "2 members", "owner alice")), out)

    rc, out = imbo("carol", "group", "apply", str(g), "--note", "T613 想加入")
    na = notice("alice", f"carol ({C}) asked to join group t613", "imbo contact requests")
    nb = notice("bob", f"carol ({C}) asked to join group t613", "imbo contact requests")
    check(613, 5, "carol asks; alice and bob are told", rc == 0 and "Asked to join t613" in out
          and "owner or an admin will answer" in out and na and nb, f"{out} | alice: {na} | bob: {nb}")

    _, out = imbo("carol", "group", "search", "t613")
    _, reqs = imbo("bob", "contact", "requests")
    m = re.search(rf"#(\d+)\s+carol \({C}\)\s+wants to join t613 #{g}", reqs)
    rid = m.group(1) if m else None
    check(613, 6, "marked applied; bob's list has it with the note", "applied" in out and rid
          and "T613 想加入" in reqs, f"{out} | {reqs}")

    rc, out = imbo("bob", "contact", "accept", str(rid))
    nc = notice("carol", "let you into group t613", "imbo group sessions")
    check(613, 7, "bob lets carol in; carol is told how to pick sessions", rc == 0 and f"carol ({C}) is in t613 now" in out
          and nc, f"{out} | carol: {nc}")

    rc, out = imbo("alice", "group", "admin", "t613", str(B), "--remove")
    _, listed = imbo("alice", "group", "list")
    check(613, 8, "bob is no longer an admin", rc == 0 and "bob is no longer an admin of t613" in out
          and "bob admin" not in listed, f"{out} | {listed}")

    rc, out = imbo("alice", "group", "discoverable", "t613", "off")
    _, found = imbo("carol", "group", "search", "t613")
    check(613, 9, "discoverable off; carol finds nothing", rc == 0 and f"t613 (#{g}) can no longer be found" in out
          and "No group found" in found and "owner lets anyone find it" in found, f"{out} | {found}")


def tc633():
    P.up("alice:bob,alice:carol,bob:carol")
    ids = P.ids()
    B, C = ids["bob"], ids["carol"]
    g = P.make_group("t633", [("bob", B), ("carol", C)])
    imbo("alice", "group", "remove", "t633", str(B), "--yes")

    rc, out = imbo("carol", "group", "invite", "t633", str(B))
    check(633, 1, "a member can't invite the removed bob", rc == 1 and "Removed from this group" in out
          and "Only its owner and admins can invite them back." in out, f"exit {rc}: {out}")

    rc1, out1 = imbo("alice", "group", "invite", "t633", str(B))
    rc2, out2 = imbo("bob", "group", "join", "t633")
    check(633, 2, "the owner invites him, he joins", rc1 == 0 and rc2 == 0 and "t633" in out2,
          f"exit {rc1}: {out1} | exit {rc2}: {out2}")

    got = [imbo("bob", "group", "leave", "t633", "--yes"), imbo("carol", "group", "invite", "t633", str(B)),
           imbo("bob", "group", "join", "t633")]
    check(633, 3, "he leaves; a member invites him and he joins", all(rc == 0 for rc, _ in got),
          " | ".join(f"exit {rc}: {o}" for rc, o in got))

    got = [imbo("alice", "group", "remove", "t633", str(B), "--yes"), imbo("alice", "group", "invite", "t633", str(B)),
           imbo("alice", "group", "transfer", "t633", str(C), "--yes")]
    rc, out = imbo("bob", "group", "join", "t633")
    check(633, 4, "removed, invited by alice who then hands it to carol: he can't join", all(r == 0 for r, _ in got)
          and rc == 1 and "Removed from this group" in out,
          " | ".join(f"exit {r}: {o}" for r, o in got) + f" | bob exit {rc}: {out}")

    _, out = imbo("carol", "group", "list")
    line = next((l.strip() for l in out.splitlines() if l.strip().startswith("members")), "")
    check(633, 5, "members alice and carol, the owner", re.sub(r"\s+", " ", line) == "members alice, carol owner",
          f"{line} | g #{g}")


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["610", "613", "629", "631", "633"]
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
