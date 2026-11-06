#!/usr/bin/env python3
"""TC-610 to TC-613 (docs/TEST.md): group admins, groups anyone can find and
asking to join one (FLOW-GROUP-DISCOVER, FLOW-GROUP-APPROVE, FLOW-GROUP-ADMIN,
RULE-023, RULE-024, API-087..090), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/group-tests.py <RUN-ID> [610 611 612 613]

Each case starts its own server and seed the way parity-tests.py does and
writes its lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if
set, on the first line); exits 1 when any step failed.
"""

import http.client
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
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


def tc611():
    P.up("alice:bob,alice:carol")
    dave("bob")
    ids = P.ids()
    B, C = ids["bob"], ids["carol"]
    g = P.make_group("t611", [("bob", B)])
    api("alice", "PATCH", f"/v1/groups/{g}", {"discoverable": True})
    api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    S = "55555555-aaaa-bbbb-cccc-000000611001"
    imbo("alice", "dev", "fake-session", S, "--title", "a611")

    s1, b1 = apply("carol", g, "T611 carol")
    s2, b2 = apply(DAVE, g, "T611 dave")
    r1, r2 = b1.get("request", {}).get("id"), b2.get("request", {}).get("id")
    check(611, 1, "carol and dave ask", s1 == 201 and s2 == 201, f"{s1} #{r1}; {s2} #{r2}")

    _, bob_in = api("bob", "GET", "/v1/requests?include=join")
    _, agent_in = api("alice", "GET", "/v1/requests?include=join", session=S)
    notes = {r["id"]: r.get("note") for r in joins(bob_in)}
    check(611, 2, "the admin sees both with notes; the owner's session sees none",
          notes == {r1: "T611 carol", r2: "T611 dave"} and not joins(agent_in), f"bob: {notes}; session: {joins(agent_in)}")

    s, b = api("alice", "POST", f"/v1/requests/{r1}/accept", session=S)
    check(611, 3, "the owner's session can't accept", s == 404 and typ(b) == "/errors/not-found", f"{s} {typ(b)}")

    s, b = api("bob", "POST", f"/v1/requests/{r1}/accept")
    gv = group("carol", g)
    check(611, 4, "the admin accepts: carol is in, no sessions picked", s == 200
          and b.get("request", {}).get("status") == "accepted" and gv.get("my_sessions") == [],
          f"{s} {b.get('request', {}).get('status')}; carol's t611 {gv.get('name')} my_sessions={gv.get('my_sessions')}")

    s, b = api("alice", "POST", f"/v1/requests/{r1}/accept")
    check(611, 5, "already handled", s == 409 and typ(b) == "/errors/request-closed", f"{s} {typ(b)}")

    s, b = api("carol", "POST", f"/v1/requests/{r2}/accept")
    check(611, 6, "a member can't accept", s == 404 and typ(b) == "/errors/not-found", f"{s} {typ(b)}")

    s, b = api("alice", "POST", f"/v1/requests/{r2}/decline")
    _, out = api(DAVE, "GET", "/v1/requests?direction=out&include=join&status=all")
    mine = {r["id"]: r["status"] for r in joins(out)}
    check(611, 7, "the owner declines; dave sees it declined", s == 200
          and b.get("request", {}).get("status") == "declined" and mine.get(r2) == "declined", f"{s}; dave {mine}")

    s, b = apply(DAVE, g)
    check(611, 8, "declined, so dave can ask again", s == 201, f"{s} #{b.get('request', {}).get('id')}")

    s1, _ = api("alice", "DELETE", f"/v1/groups/{g}/members/{C}")
    s2, b2 = apply("carol", g)
    r4 = b2.get("request", {}).get("id")
    check(611, 9, "carol removed, then asks again", s1 == 204 and s2 == 201, f"{s1}; {s2} #{r4}")

    s, b = api("bob", "POST", f"/v1/requests/{r4}/accept")
    check(611, 10, "the admin can't let a removed one back", s == 403 and typ(b) == "/errors/removed-from-group",
          f"{s} {typ(b)}")

    s, b = api("alice", "POST", f"/v1/requests/{r4}/accept")
    gv = group("alice", g)
    check(611, 11, "the owner can", s == 200 and "carol" in roles(gv), f"{s}; members {roles(gv)}")


def tc612():
    P.up("alice:bob,alice:carol")
    ids = P.ids()
    A, B, C, D = ids["alice"], ids["bob"], ids["carol"], dave("alice")
    g = P.make_group("t612", [("bob", B), ("carol", C), (f"{L}/{DAVE}", D)])
    S = "55555555-aaaa-bbbb-cccc-000000612001"
    imbo("bob", "dev", "fake-session", S, "--title", "b612")

    s, b = api("bob", "PUT", f"/v1/groups/{g}/admins/{C}")
    check(612, 1, "a member can't make admins", s == 403 and typ(b) == "/errors/not-owner", f"{s} {typ(b)}")

    s1, _ = api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    s2, b2 = api("alice", "PUT", f"/v1/groups/{g}/admins/{B}")
    r = roles(b2.get("group", {}))
    check(612, 2, "the owner makes bob an admin, twice", s1 == 200 and s2 == 200
          and r == {"alice": "owner", "bob": "admin", "carol": "member", "dave": "member"}, f"{s1} {s2} {r}")

    (s1, b1), (s2, b2) = api("alice", "PUT", f"/v1/groups/{g}/admins/{A}"), api("alice", "PUT", f"/v1/groups/{g}/admins/99999999")
    check(612, 3, "not the owner herself, not a stranger", s1 == 400 and typ(b1) == "/errors/invalid-request"
          and s2 == 404 and typ(b2) == "/errors/not-found", f"{s1} {typ(b1)}; {s2} {typ(b2)}")

    s, _ = api("bob", "DELETE", f"/v1/groups/{g}/members/{C}")
    box = imbo("carol", "inbox", "--all")[1]
    check(612, 4, "the admin removes carol, the notice names bob", s == 204
          and f"You were removed from group t612 (#{g}) by bob." in box, f"{s}; {box[-300:]}")

    s0, _ = api("alice", "PUT", f"/v1/groups/{g}/admins/{D}")
    (s1, b1), (s2, b2) = api("bob", "DELETE", f"/v1/groups/{g}/members/{D}"), api("bob", "DELETE", f"/v1/groups/{g}/members/{A}")
    check(612, 5, "the admin can't remove another admin or the owner", s0 == 200
          and s1 == 403 and typ(b1) == "/errors/not-owner" and s2 == 403 and typ(b2) == "/errors/not-owner",
          f"{s0}; {s1} {typ(b1)}; {s2} {typ(b2)}")

    got = [api("bob", "PATCH", f"/v1/groups/{g}", {"name": "x"}), api("bob", "DELETE", f"/v1/groups/{g}/admins/{D}"),
           api("bob", "PUT", f"/v1/groups/{g}/owner", {"account_id": B}), api("bob", "DELETE", f"/v1/groups/{g}")]
    check(612, 6, "the admin can't rename, unset admins, transfer or dissolve",
          all(s == 403 and typ(b) == "/errors/not-owner" for s, b in got), "; ".join(f"{s} {typ(b)}" for s, b in got))

    s1, _ = api("alice", "DELETE", f"/v1/groups/{g}/admins/{D}")
    s2, b2 = api("alice", "DELETE", f"/v1/groups/{g}/admins/{D}")
    check(612, 7, "the owner unsets dave, twice", s1 == 200 and s2 == 200 and roles(b2.get("group", {})).get("dave") == "member",
          f"{s1} {s2} {roles(b2.get('group', {}))}")

    s, b = api("alice", "PUT", f"/v1/groups/{g}/owner", {"account_id": B})
    r = roles(b.get("group", {}))
    check(612, 8, "handed to the admin: bob owns it, alice is a member, no admins", s == 200
          and r == {"alice": "member", "bob": "owner", "dave": "member"}, f"{s} {r}")

    s, b = api("bob", "PUT", f"/v1/groups/{g}/admins/{D}", session=S)
    check(612, 9, "an agent can't make admins", s == 403 and typ(b) == "/errors/forbidden", f"{s} {typ(b)}")

    s1, _ = api("bob", "PUT", f"/v1/groups/{g}/admins/{D}")
    s2, _ = api(DAVE, "DELETE", f"/v1/groups/{g}/members/me")
    r = roles(group("bob", g))
    check(612, 10, "an admin leaves like anyone", s1 == 200 and s2 == 204 and "dave" not in r, f"{s1} {s2} {r}")


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


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["610", "611", "612", "613"]
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
