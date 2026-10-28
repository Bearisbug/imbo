#!/usr/bin/env python3
"""TC-171 to TC-192 (docs/TEST.md): avatars, blocks, reports, bans, the
admin endpoints and the store reviewers' sign-in, against the local server,
with real HTTP requests.

    . crates/scripts/local-env.sh
    export IMBO_PUSH_LOG_ONLY=1 IMBO_WEB_ORIGIN=https://app.imbo.dev \\
           IMBO_ADMIN_EMAILS=admin@imbo.test IMBO_REPORT_EMAIL=reports@imbo.test
    crates/scripts/local-env.sh up alice:bob,alice:carol
    python3 crates/scripts/safety-tests.py <RUN-ID> [TC numbers…]

Prints PASS or FAIL for every step and writes each case's lines to
docs/test-runs/<run>-tc-NNN.txt; exits 1 when any step failed. The cases
build on each other and run in order; with TC numbers given, only those
run (each still needs the ones before it). TC-189 needs Waffo: run it last,
alone, after `. crates/scripts/billing-env.sh` and `local-env.sh server`.

Needs macOS `sips` to make and read the test pictures. Verification codes,
tokens and cookies are never written out.
"""

import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import shutil
import threading
import time
import traceback
import urllib.error
import urllib.request
import zlib

L, IMBO, S, M = os.environ["L"], os.environ["IMBO"], os.environ["S"], os.environ["M"]
HERE = os.path.dirname(os.path.abspath(__file__))
U = "http://127.0.0.1:18080"
WEB = "https://app.imbo.dev"
REPORT_EMAIL = "reports@imbo.test"
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
FAILS = 0
CODE_AT = {}
SECRETS = []
OUT = []
PIC = tempfile.mkdtemp(prefix="imbo-safety-")
IP = [0]


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
    mark = "PASS" if cond else "FAIL"
    say(f"{mark} TC-{tc} #{n} {what}")
    if got is not None:
        seen = repr(got)
        say("     got " + (seen if not cond or len(seen) <= 600 else seen[:600] + " …"))
    if not cond:
        FAILS += 1
    return cond


def call(method, path, token=None, body=None, cookie=None, origin=False, raw=None,
         ctype=None, headers=None):
    """Returns (status, parsed body, headers)."""
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    r = urllib.request.Request(path if path.startswith("http") else U + path, data=data, method=method)
    if body is not None:
        r.add_header("content-type", "application/json")
    if ctype:
        r.add_header("content-type", ctype)
    if token:
        r.add_header("authorization", "Bearer " + token)
    if cookie:
        r.add_header("cookie", cookie)
    if origin:
        r.add_header("origin", WEB)
    for k, v in (headers or {}).items():
        r.add_header(k, v)
    try:
        with HTTP.open(r, timeout=30) as res:
            raw_b, status, h = res.read(), res.status, res.headers
    except urllib.error.HTTPError as e:
        raw_b, status, h = e.read(), e.code, e.headers
    if h.get("content-type", "").startswith("image/"):
        return status, raw_b, h
    try:
        parsed = json.loads(raw_b) if raw_b else None
    except ValueError:
        parsed = raw_b.decode(errors="replace")
    return status, parsed, h


def problem(res, status, kind):
    s, b, _ = res
    return s == status and isinstance(b, dict) and b.get("type") == f"/errors/{kind}"


def brief(res):
    s, b, _ = res
    if isinstance(b, dict) and "type" in b:
        return (s, b["type"], b.get("detail"))
    return (s, b if not isinstance(b, bytes) else f"<{len(b)} bytes>")


def imbo(name, *args, env=None):
    e = {**os.environ, "IMBO_HOME": f"{L}/{name}", **(env or {})}
    p = subprocess.run([IMBO, *args], env=e, capture_output=True, text=True, timeout=90)
    return p.returncode, p.stdout + p.stderr


def mcp(name, session, tool, args):
    p = subprocess.run(
        ["python3", M, f"{L}/{name}", tool, json.dumps(args), "--session", f"claude-code:{session}"],
        env=os.environ, capture_output=True, text=True, timeout=90,
    )
    return p.stdout + p.stderr


def device_token(name):
    text = open(f"{L}/{name}/credentials").read()
    t = re.search(r'^token = "(.*)"', text, re.M).group(1)
    SECRETS.append(t)
    return t


def log():
    return open(f"{L}/server.log", encoding="utf-8", errors="replace").read()


def ask_code(email):
    """Ask for a code (waiting out the one-a-minute limit) and read it from the log.
    Each request claims its own address, so the 20-an-hour limit per network
    isn't reached."""
    wait = 61 - (time.time() - CODE_AT.get(email, 0))
    if wait > 0:
        time.sleep(wait)
    pattern = re.compile(rf"mail-log-only code=(\d{{6}}) email={re.escape(email)}$", re.M)
    before = len(pattern.findall(log()))
    while True:
        IP[0] += 1
        r = call("POST", "/v1/auth/code", body={"email": email},
                 headers={"x-forwarded-for": f"10.17.{IP[0] // 250}.{IP[0] % 250 + 1}"})
        if not problem(r, 429, "rate-limited"):
            break
        time.sleep(r[1].get("retry_after_s", 5) + 1)
    CODE_AT[email] = time.time()
    if not 200 <= r[0] < 300:
        return None, r
    for _ in range(50):
        codes = pattern.findall(log())
        if len(codes) > before:
            return codes[-1], r
        time.sleep(0.2)
    raise SystemExit(f"no code for {email}")


def app_login(email, name=None):
    code, r = ask_code(email)
    if code is None:
        return r
    body = {"email": email, "code": code, "platform": "ios"}
    if name:
        body["name"] = name
    r = call("POST", "/v1/app/session", body=body)
    if r[0] == 200:
        SECRETS.append(r[1]["token"])
    return r


def web_login(email, name=None):
    code, r = ask_code(email)
    if code is None:
        return None
    body = {"email": email, "code": code}
    if name:
        body["name"] = name
    _, _, h = call("POST", "/v1/web/session", body=body, origin=True)
    cookie = h.get("set-cookie", "").split(";")[0]
    SECRETS.append(cookie.split("=", 1)[-1])
    return cookie


def computer_login(email, name):
    """A computer sign-in (API-002) without a daemon: returns (account id, token)."""
    code, _ = ask_code(email)
    r = call("POST", "/v1/auth/verify", body={
        "email": email, "code": code, "name": name, "device_name": f"{name}-mac",
        "platform": "macos-aarch64", "client_version": "0.2.12"})
    SECRETS.append(r[1]["token"])
    return r[1]["account"]["id"], r[1]["token"]


def ids():
    rows = [line.split() for line in open(f"{L}/ids.tsv") if line.strip()]
    return {r[0]: int(r[1]) for r in rows}


def group_id(name, owner):
    _, out = imbo(owner, "group", "list")
    for line in out.splitlines():
        if name in line:
            return int(re.search(r"#(\d+)", line).group(1))
    raise SystemExit(f"no group {name} in {owner}'s list: {out}")


def invite_id(member, gid):
    _, b, _ = call("GET", "/v1/requests", token=DEV[member])
    for r in b["requests"]:
        if r["kind"] == "group" and r.get("group", {}).get("id") == gid and r["status"] == "pending":
            return r["id"]
    return None


def conv_of(token, group=None, peer=None):
    _, b, _ = call("GET", "/v1/conversations", token=token)
    for c in b["conversations"]:
        if group and c.get("group", {}).get("id") == group:
            return c
        if peer and c.get("peer", {}).get("id") == peer:
            return c
    return None


def bodies(token, conv):
    _, b, _ = call("GET", f"/v1/conversations/{conv}/messages?limit=50", token=token)
    return [m["body"] for m in b["messages"]]


def contact(token, peer):
    _, b, _ = call("GET", "/v1/contacts", token=token)
    return next((c for c in b["contacts"] if c["id"] == peer), None)


def pushes_for(account, since):
    n = 0
    for line in log().splitlines():
        if 'event="push_logged"' in line:
            n += 1
            if n > since and f"account_id={account}" in line:
                return True
    return False


def push_count():
    return log().count('event="push_logged"')


# ---------- pictures ----------

def png(w, h, pixel):
    rows = b"".join(b"\x00" + bytes(c for x in range(w) for c in pixel(x, y)) for y in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    kind = 6 if len(pixel(0, 0)) == 4 else 2
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, kind, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def exif(orientation):
    """Little-endian TIFF: Orientation, and a GPS block with a latitude."""
    e = b"II\x2a\x00\x08\x00\x00\x00" + struct.pack("<H", 2)
    e += bytes([0x12, 0x01, 3, 0, 1, 0, 0, 0]) + struct.pack("<H", orientation) + b"\x00\x00"
    e += bytes([0x25, 0x88, 4, 0, 1, 0, 0, 0, 38, 0, 0, 0]) + b"\x00" * 4
    e += struct.pack("<H", 1) + bytes([1, 0, 2, 0, 2, 0, 0, 0]) + b"N\x00\x00\x00" + b"\x00" * 4
    return e


def jpeg(name, w, h, pixel, orientation=None):
    """A JPEG made by sips from a PNG. With `orientation`, its EXIF block is
    replaced by one carrying that orientation and a GPS latitude, as a
    phone photo has."""
    src, dst = f"{PIC}/{name}.png", f"{PIC}/{name}.jpg"
    open(src, "wb").write(png(w, h, pixel))
    subprocess.run(["sips", "-s", "format", "jpeg", src, "--out", dst], capture_output=True, check=True)
    data = open(dst, "rb").read()
    if orientation:
        segs, i = [], 2
        while data[i + 1] != 0xDA:
            n = struct.unpack(">H", data[i + 2:i + 4])[0]
            seg = data[i:i + 2 + n]
            if not (seg[1] == 0xE1 and seg[4:10] == b"Exif\x00\x00"):
                segs.append(seg)
            i += 2 + n
        payload = b"Exif\x00\x00" + exif(orientation)
        app1 = b"\xff\xe1" + struct.pack(">H", len(payload) + 2) + payload
        data = data[:2] + segs[0] + app1 + b"".join(segs[1:]) + data[i:]
        open(dst, "wb").write(data)
    return dst


def picture(path_or_bytes):
    """(width, height, pixel(x, y) -> (r, g, b)) of a JPEG, read through sips."""
    if isinstance(path_or_bytes, bytes):
        path = f"{PIC}/got-{time.time_ns()}.jpg"
        open(path, "wb").write(path_or_bytes)
    else:
        path = path_or_bytes
    bmp = path + ".bmp"
    subprocess.run(["sips", "-s", "format", "bmp", path, "--out", bmp], capture_output=True, check=True)
    b = open(bmp, "rb").read()
    off = struct.unpack_from("<I", b, 10)[0]
    w, h = struct.unpack_from("<ii", b, 18)
    bpp = struct.unpack_from("<H", b, 28)[0] // 8
    row = (w * bpp + 3) // 4 * 4
    top_down = h < 0
    h = abs(h)

    def px(x, y):
        yy = y if top_down else h - 1 - y
        i = off + yy * row + x * bpp
        return b[i + 2], b[i + 1], b[i]
    return w, h, px


def near(p, want):
    return all(abs(a - b) < 60 for a, b in zip(p, want))


RED, GREEN, BLUE = (255, 0, 0), (0, 200, 0), (0, 0, 255)


def put_avatar(path, file, ctype, token=None, cookie=None, origin=False):
    return call("PUT", path, token=token, cookie=cookie, origin=origin,
                raw=open(file, "rb").read() if isinstance(file, str) else file, ctype=ctype)


def avatar_id(url):
    return url.rsplit("/", 1)[-1].removesuffix(".jpg")


# ---------- cases ----------

def tc171():
    a, b = ID["alice"], ID["bob"]
    # Stored landscape, left half red and right half blue; orientation 6
    # turns it upright with the red half on top.
    photo = jpeg("p171", 1200, 600, lambda x, y: RED if x < 600 else BLUE, orientation=6)
    raw = open(photo, "rb").read()
    say(f"   p171.jpg: {len(raw)} bytes, EXIF {'Exif' if b'Exif' in raw else 'none'}, GPS block present: {b'N' in raw}")
    r = put_avatar("/v1/me/avatar", photo, "image/jpeg", token=TA)
    url = r[1].get("avatar_url", "") if isinstance(r[1], dict) else ""
    check(171, 1, r[0] == 200 and re.fullmatch(r"/v1/avatars/[0-9a-f-]{36}\.jpg", url) is not None,
          "alice's App puts a 1200x600 JPEG with orientation 6 and GPS: 200 with an avatar path (no IMBO_DOMAIN)",
          brief(r))
    g = call("GET", url)
    ok = g[0] == 200 and isinstance(g[1], bytes)
    w = h = 0
    top = bottom = None
    if ok:
        w, h, px = picture(g[1])
        top, bottom = px(256, 60), px(256, 450)
    check(171, 2, ok and g[2].get("content-type") == "image/jpeg"
          and g[2].get("cache-control") == "public, max-age=31536000, immutable"
          and (w, h) == (512, 512) and b"Exif" not in g[1] and near(top, RED) and near(bottom, BLUE),
          "the address, unsigned: image/jpeg cached for a year, 512x512, no EXIF, turned upright (top red, bottom blue)",
          (g[0], g[2].get("content-type"), g[2].get("cache-control"), (w, h), top, bottom))
    me = call("GET", "/v1/me", token=TA)
    c = contact(DEV["bob"], a)
    check(171, 3, me[1]["account"].get("avatar_url") == url and c and c.get("avatar_url") == url,
          "alice's /v1/me and bob's contact list show the same address", (me[1]["account"].get("avatar_url"), c))
    stripes = f"{PIC}/p171.png"
    open(stripes, "wb").write(png(300, 100, lambda x, y: RED if x < 100 else GREEN if x < 200 else BLUE))
    r2 = put_avatar("/v1/me/avatar", stripes, "image/png", token=TA)
    url2 = r2[1].get("avatar_url", "") if isinstance(r2[1], dict) else ""
    old = call("GET", url)
    g2 = call("GET", url2)
    mid = picture(g2[1])[2](256, 256) if g2[0] == 200 else None
    dims = picture(g2[1])[:2] if g2[0] == 200 else None
    check(171, 4, r2[0] == 200 and url2 != url and problem(old, 404, "not-found")
          and dims == (512, 512) and near(mid, GREEN),
          "a 300x100 PNG next: new address, the old one 404, the middle square scaled up to 512x512",
          (brief(r2), brief(old), dims, mid))
    r3 = call("PUT", "/v1/me/avatar", token=TA, raw=b"GIF89a" + b"\x00" * 64, ctype="image/gif")
    r4 = call("PUT", "/v1/me/avatar", token=TA, raw=b"\xff\xd8\xff\xe0" + b"broken" * 20, ctype="image/jpeg")
    check(171, 5, problem(r3, 422, "image-invalid") and problem(r4, 422, "image-invalid"),
          "a GIF, and broken bytes labeled image/jpeg: 422 image-invalid", (brief(r3), brief(r4)))
    big = f"{PIC}/big.jpg"
    open(big, "wb").write(b"\xff\xd8\xff\xe0" + b"\x00" * (5 * 1024 * 1024 - 3))
    out = subprocess.run(["curl", "-s", "--noproxy", "*", "-X", "PUT", f"{U}/v1/me/avatar",
                          "-H", f"authorization: Bearer {TA}", "-H", "content-type: image/jpeg",
                          "--data-binary", f"@{big}", "-w", "\n%{http_code}"],
                         capture_output=True, text=True).stdout
    check(171, 6, out.endswith("\n413") and "/errors/file-too-large" in out,
          "5242881 bytes: 413 file-too-large", out[-200:])
    r5 = put_avatar("/v1/me/avatar", stripes, "image/png", token=DEV["alice"])
    check(171, 7, problem(r5, 403, "forbidden"), "with alice's computer token: 403 forbidden", brief(r5))
    r6 = put_avatar("/v1/me/avatar", stripes, "image/png", cookie=CK["bob"], origin=True)
    r6b = put_avatar("/v1/me/avatar", stripes, "image/png", cookie=CK["bob"])
    check(171, 8, r6[0] == 200 and "avatar_url" in r6[1] and problem(r6b, 403, "forbidden"),
          "bob's web sign-in with the web Origin: 200; without it: 403", (brief(r6), brief(r6b)))
    d1 = call("DELETE", "/v1/me/avatar", token=TA)
    d2 = call("DELETE", "/v1/me/avatar", token=TA)
    me = call("GET", "/v1/me", token=TA)
    c = contact(DEV["bob"], a)
    gone = call("GET", url2)
    check(171, 9, d1[0] == 204 and d2[0] == 204 and "avatar_url" not in me[1]["account"]
          and c and "avatar_url" not in c and problem(gone, 404, "not-found"),
          "alice deletes it twice: 204 both times; no avatar_url anywhere; the address 404",
          (brief(d1), brief(d2), me[1]["account"], c, brief(gone)))
    _ = b


def tc172():
    global FRANK
    fid, ft = computer_login("frank@imbo.test", "frank")
    FRANK = (fid, ft)
    g = call("POST", "/v1/groups", token=ft, body={"name": "g172"})
    gid = g[1]["group"]["id"]
    say(f"   frank {fid} signed in from a computer, made group g172 #{gid}")
    small = f"{PIC}/p172.png"
    open(small, "wb").write(png(40, 40, lambda x, y: BLUE))
    tf = app_login("frank@imbo.test")[1]["token"]
    got = [put_avatar("/v1/me/avatar", small, "image/png", token=tf)[0] for _ in range(19)]
    check(172, 1, got == [200] * 19, "frank's App changes his avatar 19 times: 200 each", got)
    r = put_avatar(f"/v1/groups/{gid}/avatar", small, "image/png", token=ft)
    check(172, 2, r[0] == 200 and "avatar_url" in r[1],
          "the 20th change is g172's picture, from his computer token: 200", brief(r))
    r1 = put_avatar("/v1/me/avatar", small, "image/png", token=tf)
    r2 = put_avatar(f"/v1/groups/{gid}/avatar", small, "image/png", token=ft)
    check(172, 3, problem(r1, 429, "rate-limited") and problem(r2, 429, "rate-limited")
          and r1[1].get("retry_after_s", 0) > 0,
          "a 21st change, personal or group: 429 rate-limited with retry_after_s", (brief(r1), brief(r2)))
    d = call("DELETE", "/v1/me/avatar", token=tf)
    check(172, 4, d[0] == 204, "deleting isn't counted: 204", brief(d))


def tc173():
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    imbo("alice", "group", "create", "g173")
    gid = group_id("g173", "alice")
    imbo("alice", "group", "invite", f"#{gid}", str(b))
    imbo("bob", "group", "join", f"#{gid}")
    photo = jpeg("p173", 600, 600, lambda x, y: GREEN)
    say(f"   g173 #{gid}: alice owner, bob member")
    code, out = imbo("alice", "group", "avatar", f"#{gid}", photo)
    url = (re.search(r"(http\S+\.jpg)", out) or [None, ""])[1]
    check(173, 1, code == 0 and "Changed the picture of" in out and url,
          "alice: imbo group avatar '#<g>' p173.jpg exits 0 and prints the address", (code, out))
    _, groups, _ = call("GET", "/v1/groups", token=DEV["bob"])
    gv = next((x for x in groups["groups"] if x["id"] == gid), {})
    pic = call("GET", url) if url else (0, None, {})
    check(173, 2, U + gv.get("avatar_url", "") == url and pic[0] == 200 and picture(pic[1])[:2] == (512, 512),
          "bob's group list has the path the command printed after the server address; it serves a 512x512 JPEG",
          (gv, brief(pic)))
    code, out = imbo("bob", "group", "avatar", f"#{gid}", photo)
    r = put_avatar(f"/v1/groups/{gid}/avatar", photo, "image/jpeg", token=TB)
    check(173, 3, code != 0 and "owner" in out and problem(r, 403, "not-owner"),
          "bob, a member: the command fails; his App gets 403 not-owner", (code, out, brief(r)))
    sid = "17317317-aaaa-bbbb-cccc-000000173001"
    imbo("alice", "dev", "fake-session", sid, "--title", "a173")
    code, out = imbo("alice", "group", "avatar", f"#{gid}", photo, env={"CLAUDE_CODE_SESSION_ID": sid})
    check(173, 4, code != 0 and "outside any agent session" in out,
          "the same command inside alice's session a173: fails, run it outside any agent session", (code, out))
    heic = f"{PIC}/p173.heic"
    open(heic, "wb").write(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic" + b"\x00" * 200)
    code, out = imbo("alice", "group", "avatar", f"#{gid}", heic)
    check(173, 5, code != 0 and "Not a JPEG or PNG image" in out and "convert it to JPEG or PNG first" in out,
          "a HEIC file: fails, not a JPEG or PNG, convert it first", (code, out))
    r = put_avatar(f"/v1/groups/{gid}/avatar", photo, "image/jpeg", token=TC)
    check(173, 6, problem(r, 403, "not-member"), "carol, not in g173, from her App: 403 not-member", brief(r))
    code, out = imbo("alice", "group", "avatar", f"#{gid}", "--remove")
    code2, _ = imbo("alice", "group", "avatar", f"#{gid}", "--remove")
    _, groups, _ = call("GET", "/v1/groups", token=DEV["bob"])
    gv = next((x for x in groups["groups"] if x["id"] == gid), {})
    old = call("GET", url)
    check(173, 7, code == 0 and code2 == 0 and "avatar_url" not in gv and problem(old, 404, "not-found"),
          "--remove, twice: both exit 0; no avatar_url for g173; the old address 404", (out, code2, gv, brief(old)))
    _, out = imbo("alice", "group", "avatar", f"#{gid}", photo)
    url2 = (re.search(r"(http\S+\.jpg)", out) or [None, ""])[1]
    imbo("alice", "group", "dissolve", f"#{gid}", "--yes")
    gone = call("GET", url2)
    r = put_avatar(f"/v1/groups/{gid}/avatar", photo, "image/jpeg", token=TA)
    check(173, 8, url2 and problem(gone, 404, "not-found") and problem(r, 404, "not-found"),
          "set again, then dissolve g173: its address 404; changing it now: 404 not-found", (url2, brief(gone), brief(r)))
    _ = (a, c)
    return gid


def tc174():
    global DAVE
    a, b = ID["alice"], ID["bob"]
    did, dt = computer_login("dave@imbo.test", "dave")
    DAVE = (did, dt)
    say(f"   dave {did} signed in from a computer, no contacts")
    imbo("alice", "send", "bob", "T174 alice before")
    imbo("bob", "send", "alice", "T174 bob before")
    rq = call("POST", "/v1/contacts/requests", token=dt, body={"account_id": b, "note": "T174 dave here"})
    imbo("alice", "group", "create", "g174")
    g174 = group_id("g174", "alice")
    imbo("alice", "group", "invite", f"#{g174}", str(b))
    check(174, 1, rq[0] == 201 and invite_id("bob", g174) is not None,
          "before: alice and bob each wrote the other; dave asked bob; alice invited bob to g174", (brief(rq), g174))
    r1 = call("POST", "/v1/blocks", token=TB, body={"account_id": a})
    r2 = call("POST", "/v1/blocks", token=TB, body={"account_id": a})
    blk = r1[1].get("block", {}) if isinstance(r1[1], dict) else {}
    check(174, 2, r1[0] == 201 and blk.get("account", {}).get("id") == a and blk["account"].get("name") == "alice"
          and r2[0] == 200 and r2[1]["block"]["created_at"] == blk.get("created_at"),
          "bob's App blocks alice: 201; again: 200 with the same block", (brief(r1), brief(r2)))
    r3 = call("POST", "/v1/blocks", token=TB, body={"account_id": did})
    _, reqs, _ = call("GET", "/v1/requests", token=TB)
    pending = [(x["from"]["id"], x["kind"]) for x in reqs["requests"] if x["status"] == "pending"]
    check(174, 3, r3[0] == 201 and (did, "contact") not in pending and (a, "group") not in pending,
          "bob blocks dave too: 201; dave's request and alice's g174 invite are no longer pending", (brief(r3), pending))
    _, ib = imbo("bob", "inbox")
    _, ia = imbo("alice", "inbox")
    check(174, 4, "T174 alice before" not in ib and "T174 bob before" not in ia,
          "bob's inbox lacks T174 alice before; alice's lacks T174 bob before", (ib[-300:], ia[-300:]))
    code, out = imbo("alice", "send", "bob", "T174 after")
    r = call("POST", "/v1/messages", token=DEV["alice"], body={"to": str(b), "body": "T174 api", "client_msg_id": "t174-1"})
    check(174, 5, code != 0 and "bob doesn't accept your messages." in out and problem(r, 403, "blocked"),
          "alice writes bob: the command fails, bob doesn't accept your messages; the API says 403 blocked", (code, out, brief(r)))
    r1 = call("POST", "/v1/messages", token=DEV["bob"], body={"to": str(a), "body": "T174 b", "client_msg_id": "t174-2"})
    r2 = call("POST", "/v1/messages", cookie=CK["bob"], origin=True,
              body={"to": str(a), "body": "T174 web", "client_msg_id": "t174-3"})
    check(174, 6, problem(r1, 403, "unblock-first") and r1[1]["detail"] == "You blocked alice. Unblock them first."
          and problem(r2, 403, "unblock-first"),
          "bob writes alice from his computer and from the web: 403 unblock-first", (brief(r1), brief(r2)))
    r1 = call("POST", "/v1/contacts/requests", token=dt, body={"account_id": b})
    r2 = call("POST", "/v1/contacts/requests", token=DEV["bob"], body={"account_id": did})
    code, out = imbo("alice", "group", "invite", f"#{g174}", str(b))
    check(174, 7, problem(r1, 403, "blocked") and problem(r2, 403, "unblock-first")
          and code != 0 and "doesn't accept" in out,
          "dave asks bob: 403 blocked; bob asks dave: 403 unblock-first; alice invites bob again: refused",
          (brief(r1), brief(r2), out))
    cb = contact(TB, a)
    ca = contact(TA, b)
    check(174, 8, cb and cb.get("blocked") is True and ca and "blocked" not in ca,
          "bob's contacts keep alice, marked blocked; alice's list shows bob as before", (cb, ca))
    _, bl, _ = call("GET", "/v1/blocks", token=TB)
    order = [x["account"]["id"] for x in bl["blocks"]]
    check(174, 9, order == [did, a], "bob's block list: dave, then alice (newest first)", bl)
    r1 = call("POST", "/v1/blocks", token=TB, body={"account_id": b})
    r2 = call("POST", "/v1/blocks", token=TB, body={"account_id": 9999999})
    r3 = call("POST", "/v1/blocks", token=DEV["bob"], body={"account_id": a})
    check(174, 10, problem(r1, 400, "invalid-request") and problem(r2, 404, "account-not-found")
          and problem(r3, 403, "forbidden"),
          "blocking himself: 400; account 9999999: 404 account-not-found; from his computer: 403", (brief(r1), brief(r2), brief(r3)))


def tc175():
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    u = call("DELETE", f"/v1/blocks/{a}", token=TB)
    sa, sb, sc = ("17517517-aaaa-bbbb-cccc-000000175a01", "17517517-aaaa-bbbb-cccc-000000175b01",
                  "17517517-aaaa-bbbb-cccc-000000175c01")
    imbo("alice", "dev", "fake-session", sa, "--title", "a175")
    imbo("bob", "dev", "fake-session", sb, "--title", "b175")
    imbo("carol", "dev", "fake-session", sc, "--title", "c175")
    imbo("alice", "group", "create", "g175", "--sessions", sa[-6:])
    gid = group_id("g175", "alice")
    imbo("alice", "group", "invite", f"#{gid}", str(b))
    imbo("alice", "group", "invite", f"#{gid}", str(c))
    imbo("bob", "group", "join", f"#{gid}", "--sessions", sb[-6:])
    imbo("carol", "group", "join", f"#{gid}", "--sessions", sc[-6:])
    s0 = call("POST", "/v1/messages", token=DEV["alice"],
              body={"to": f"#{gid}", "body": "T175 before block", "client_msg_id": "t175-0"})
    say(f"   bob unblocked alice first: {u[0]}; g175 #{gid} with sessions a175, b175, c175; "
        f"alice's first group message reached {s0[1].get('recipients')} session(s)")
    r = call("POST", "/v1/blocks", token=TB, body={"account_id": a})
    ib = mcp("bob", sb, "inbox", {})
    ic = mcp("carol", sc, "inbox", {})
    check(175, 1, r[0] == 201 and "T175 before block" not in ib and "T175 before block" in ic,
          "bob blocks alice; session b175's inbox lacks alice's undelivered group message, c175 has it", (brief(r), ib[-300:], ic[-300:]))
    s1 = call("POST", "/v1/messages", token=DEV["alice"],
              body={"to": f"#{gid}", "body": "T175 after block", "client_msg_id": "t175-1"})
    check(175, 2, s1[0] == 201 and s1[1]["recipients"] == s0[1]["recipients"] - 1
          and s1[1].get("inactive_members", 0) == 0,
          "alice writes g175 again: 201, one session fewer, and nobody counted as skipped", (brief(s0), brief(s1)))
    ib = mcp("bob", sb, "inbox", {})
    ic = mcp("carol", sc, "inbox", {})
    check(175, 3, "T175 after block" not in ib and "T175 after block" in ic,
          "b175 doesn't get it, c175 does", (ib[-300:], ic[-300:]))
    conv = conv_of(TB, group=gid)
    seen_b = bodies(TB, conv["id"]) if conv else []
    seen_c = bodies(TC, conv["id"]) if conv else []
    check(175, 4, conv and "T175 after block" not in seen_b and "T175 before block" not in seen_b
          and "T175 after block" in seen_c and conv.get("last_message", {}).get("body") != "T175 after block",
          "bob's view of g175 hides alice's messages, carol's shows them; bob's list doesn't show alice's as the last",
          (conv, seen_b, seen_c))
    r = call("POST", "/v1/messages", token=DEV["bob"],
             body={"to": f"#{gid}", "body": "T175 bob to the group", "client_msg_id": "t175-2"})
    ia = mcp("alice", sa, "inbox", {})
    check(175, 5, r[0] == 201 and "T175 bob to the group" in ia,
          "bob writes g175: alice's session a175 gets it", (brief(r), ia[-300:]))
    d1 = call("DELETE", f"/v1/blocks/{a}", token=TB)
    d2 = call("DELETE", f"/v1/blocks/{a}", token=TB)
    seen_b = bodies(TB, conv["id"]) if conv else []
    ib = mcp("bob", sb, "inbox", {})
    check(175, 6, d1[0] == 204 and problem(d2, 404, "not-found") and "T175 after block" in seen_b
          and "T175 after block" not in ib,
          "bob unblocks alice: 204, again 404 not-found; g175 shows alice's messages again; b175 isn't sent them",
          (brief(d1), brief(d2), seen_b, ib[-200:]))
    code, _ = imbo("alice", "send", "bob", "T175 after unblock")
    _, ib = imbo("bob", "inbox")
    check(175, 7, code == 0 and "T175 after unblock" in ib, "alice writes bob again: it arrives", ib[-300:])
    return gid


def report_of(rid):
    return call("GET", f"/v1/admin/reports/{rid}", cookie=CK["admin"])


def tc176():
    global R_CAROL, R_DAVE
    a, c = ID["alice"], ID["carol"]
    did, dt = DAVE
    for i in range(1, 21):
        call("POST", "/v1/messages", token=DEV["carol"],
             body={"to": str(a), "body": f"T176 c{i:02}", "client_msg_id": f"t176-c{i}"})
    imbo("alice", "send", "carol", "T176 alice says stop")
    att = f"{PIC}/t176.txt"
    open(att, "w").write("not for the admin\n" * 10)
    imbo("carol", "send", "alice", "T176 c21", "--file", att)
    call("POST", "/v1/contacts/requests", token=dt, body={"account_id": a, "note": "T176 dave note"})
    r = call("POST", "/v1/reports", token=TA, body={"account_id": c, "reason": "harassment", "note": "T176 keeps writing"})
    R_CAROL = r[1].get("report", {}).get("id") if isinstance(r[1], dict) else None
    check(176, 1, r[0] == 201 and isinstance(R_CAROL, int) and "created_at" in r[1]["report"],
          "alice's App reports carol for harassment with a note: 201 {report: {id, created_at}}", brief(r))
    time.sleep(1)
    mails = [line for line in log().splitlines() if f"mail-log-only report={R_CAROL} " in line]
    check(176, 2, len(mails) == 1 and f"to={REPORT_EMAIL}" in mails[0]
          and f"link={WEB}/admin?report={R_CAROL}" in mails[0]
          and "T176" not in mails[0] and "carol" not in mails[0] and "alice" not in mails[0],
          "one mail to reports@imbo.test with only the admin link, no names or messages", mails)
    v = report_of(R_CAROL)
    rep = v[1].get("report", {}) if isinstance(v[1], dict) else {}
    msgs = rep.get("messages", [])
    body_list = [m["body"] for m in msgs]
    last = msgs[-1] if msgs else {}
    check(176, 3, v[0] == 200 and rep.get("reason") == "harassment" and rep.get("note") == "T176 keeps writing"
          and rep["target"]["id"] == c and rep["target"].get("email") == "carol@imbo.test"
          and rep["reporter"]["id"] == a and len(msgs) == 20 and body_list[0] == "T176 c03"
          and "T176 c02" not in body_list and "T176 alice says stop" in body_list
          and last.get("body") == "T176 c21" and last.get("files") == [{"name": "t176.txt", "size": 180}]
          and all(m["kind"] == "message" for m in msgs),
          "the admin sees the reason, note, carol's email and the last 20 messages of both: c03…c20, alice's, c21 with t176.txt (180 bytes)",
          (brief(v), body_list, last))
    r = call("POST", "/v1/reports", token=TA, body={"account_id": did, "reason": "spam"})
    R_DAVE = r[1]["report"]["id"] if r[0] == 201 else None
    v = report_of(R_DAVE) if R_DAVE else (0, {}, {})
    msgs = v[1].get("report", {}).get("messages", []) if isinstance(v[1], dict) else []
    check(176, 4, r[0] == 201 and [(m["kind"], m["from"]["id"], m["body"]) for m in msgs]
          == [("request_note", did, "T176 dave note")],
          "alice reports dave, who only sent a request: the note is attached as request_note", (brief(r), msgs))
    r = call("POST", "/v1/reports", token=TA, body={"account_id": c, "reason": "spam"})
    check(176, 5, problem(r, 409, "already-reported"), "reporting carol again: 409 already-reported", brief(r))
    rs = [call("POST", "/v1/reports", token=TA, body={"account_id": c, "reason": "abuse"}),
          call("POST", "/v1/reports", token=TA, body={"account_id": c, "reason": "other", "note": "字" * 501}),
          call("POST", "/v1/reports", token=TA, body={"account_id": a, "reason": "other"}),
          call("POST", "/v1/reports", token=TA, body={"account_id": 9999999, "reason": "other"}),
          call("POST", "/v1/reports", token=DEV["alice"], body={"account_id": c, "reason": "other"})]
    check(176, 6, problem(rs[0], 400, "invalid-request") and problem(rs[1], 400, "invalid-request")
          and problem(rs[2], 400, "invalid-request") and problem(rs[3], 404, "account-not-found")
          and problem(rs[4], 403, "forbidden"),
          "reason abuse, a 501-character note, herself: 400; account 9999999: 404; from her computer: 403",
          [brief(x) for x in rs])
    call("POST", "/v1/messages", token=DEV["carol"], body={"to": str(a), "body": "T176 after report", "client_msg_id": "t176-z"})
    v = report_of(R_CAROL)
    after = [m["body"] for m in v[1]["report"]["messages"]]
    check(176, 7, after == body_list, "carol writes again; the report's messages stay as they were", after[-3:])


def tc177(g173):
    global R_BOB_G
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    did, dt = DAVE
    imbo("alice", "group", "create", "g177")
    gid = group_id("g177", "alice")
    for m in (b, c):
        imbo("alice", "group", "invite", f"#{gid}", str(m))
    imbo("bob", "group", "join", f"#{gid}")
    imbo("carol", "group", "join", f"#{gid}")
    for who, text in (("bob", "T177 bob one"), ("carol", "T177 carol"), ("alice", "T177 alice"), ("bob", "T177 bob two")):
        call("POST", "/v1/messages", token=DEV[who], body={"to": f"#{gid}", "body": text, "client_msg_id": f"t177-{text}"})
    call("POST", "/v1/messages", token=DEV["bob"], body={"to": str(a), "body": "T177 bob direct", "client_msg_id": "t177-d"})
    say(f"   g177 #{gid}: alice owner, bob and carol members")
    r = call("POST", "/v1/reports", token=TA, body={"account_id": b, "reason": "spam", "group_id": gid})
    R_BOB_G = r[1]["report"]["id"] if r[0] == 201 else None
    v = report_of(R_BOB_G) if R_BOB_G else (0, {}, {})
    rep = v[1].get("report", {}) if isinstance(v[1], dict) else {}
    check(177, 1, r[0] == 201 and [m["body"] for m in rep.get("messages", [])] == ["T177 bob one", "T177 bob two"]
          and rep.get("group") == {"id": gid, "name": "g177"},
          "alice reports bob in g177: only bob's two messages in g177 are attached, with the group", rep)
    res = [None, None]

    def send(i):
        res[i] = call("POST", "/v1/reports", token=TB, body={"account_id": c, "reason": "other"})
    th = [threading.Thread(target=send, args=(i,)) for i in (0, 1)]
    [t.start() for t in th]
    [t.join() for t in th]
    codes = sorted(x[0] for x in res)
    _, lst, _ = call("GET", "/v1/admin/reports?status=all", cookie=CK["admin"])
    mine = [x for x in lst["reports"] if x["reporter"]["id"] == b and x["target"]["id"] == c]
    check(177, 2, codes == [201, 409] and len(mine) == 1 and any(problem(x, 409, "already-reported") for x in res),
          "bob's App reports carol twice at once: one 201, one 409 already-reported; one report stored",
          ([brief(x) for x in res], len(mine)))
    r1 = call("POST", "/v1/reports", token=TA, body={"account_id": b, "reason": "spam", "group_id": g173})
    td = app_login("dave@imbo.test")[1]["token"]
    DAVE_APP[0] = td
    r2 = call("POST", "/v1/reports", token=td, body={"account_id": c, "reason": "spam", "group_id": gid})
    check(177, 3, problem(r1, 404, "not-found") and problem(r2, 403, "not-member"),
          "with dissolved g173: 404 not-found; dave, not in g177: 403 not-member", (brief(r1), brief(r2)))
    made = []
    for i in range(1, 9):
        r = app_login(f"e{i}@imbo.test", f"e{i}")
        made.append(r[1]["account"]["id"])
        E_APP[i - 1] = r[1]["token"]
    E.extend(made)
    got = [call("POST", "/v1/reports", token=TA, body={"account_id": x, "reason": "other"})[0] for x in made[:7]]
    over = call("POST", "/v1/reports", token=TA, body={"account_id": made[7], "reason": "other"})
    check(177, 4, got == [201] * 7 and problem(over, 429, "rate-limited"),
          "alice already filed 3 today; 7 more to e1…e7: 201 each; the 11th, to e8: 429 rate-limited",
          (got, brief(over)))
    return gid


def tc178():
    rs = [call("GET", "/v1/admin/reports"),
          call("GET", "/v1/admin/reports", cookie=CK["bob"]),
          call("GET", "/v1/admin/stats", cookie=CK["bob"]),
          call("POST", f"/v1/admin/accounts/{ID['carol']}/ban", cookie=CK["bob"], origin=True),
          call("GET", "/v1/admin/reports", token=TB),
          call("GET", "/v1/admin/reports", token=DEV["bob"])]
    check(178, 1, problem(rs[0], 401, "unauthorized"), "no sign-in: 401 unauthorized", brief(rs[0]))
    check(178, 2, all(problem(x, 404, "not-found") for x in rs[1:]),
          "bob's web sign-in (reports, stats, ban), App and computer: 404 not-found each", [brief(x) for x in rs[1:]])
    m1 = call("GET", "/v1/me", cookie=CK["admin"])
    m2 = call("GET", "/v1/me", cookie=CK["bob"])
    check(178, 3, m1[1]["account"].get("admin") is True and "admin" not in m2[1]["account"],
          "/v1/me: admin true for the admin's web sign-in, absent for bob's", (m1[1]["account"], m2[1]["account"]))
    ta = app_login("admin@imbo.test")[1]["token"]
    r = call("GET", "/v1/admin/reports", token=ta)
    m = call("GET", "/v1/me", token=ta)
    check(178, 4, problem(r, 404, "not-found") and "admin" not in m[1]["account"],
          "the admin's App sign-in: 404 not-found, and no admin in /v1/me", (brief(r), m[1]["account"]))
    r = call("POST", f"/v1/admin/reports/{R_DAVE}/resolve", cookie=CK["admin"], body={"action": "dismiss"})
    check(178, 5, problem(r, 403, "forbidden"), "the admin's web sign-in without the web Origin: 403 forbidden", brief(r))
    r = call("GET", "/v1/admin/nothing", cookie=CK["admin"])
    check(178, 6, problem(r, 404, "not-found"), "an unknown admin path: 404", brief(r))


def tc179():
    did, dt = DAVE
    _, lst, _ = call("GET", "/v1/admin/reports", cookie=CK["admin"])
    reps = lst["reports"]
    times = [x["created_at"] for x in reps]
    item = next((x for x in reps if x["id"] == R_BOB_G), {})
    check(179, 1, lst["total"] == len(reps) == 11 and times == sorted(times, reverse=True)
          and all(x["status"] == "open" for x in reps) and item.get("group", {}).get("name") == "g177"
          and set(item.get("reporter", {})) >= {"id", "name", "banned"},
          "admin lists open reports: 11, newest first, each with reporter, target, group for g177",
          (lst["total"], len(reps)))
    a = call("GET", "/v1/admin/reports?status=all", cookie=CK["admin"])
    p2 = call("GET", "/v1/admin/reports?page=2", cookie=CK["admin"])
    bad = call("GET", "/v1/admin/reports?status=bogus", cookie=CK["admin"])
    check(179, 2, a[1]["total"] == 11 and p2[1]["reports"] == [] and p2[1]["total"] == 11
          and problem(bad, 400, "invalid-request"),
          "status=all: 11; page 2: empty, total 11; status=bogus: 400", (a[1]["total"], p2[1], brief(bad)))
    h = call("GET", "/v1/health")
    check(179, 3, isinstance(h[1].get("oldest_open_report_h"), (int, float)) and h[1]["oldest_open_report_h"] >= 0,
          "/v1/health carries oldest_open_report_h", h[1])
    td = DAVE_APP[0]
    small = f"{PIC}/p179.png"
    open(small, "wb").write(png(64, 64, lambda x, y: RED))
    put = put_avatar("/v1/me/avatar", small, "image/png", token=td)
    v = report_of(R_DAVE)
    t = v[1]["report"]["target"]
    check(179, 4, put[0] == 200 and t.get("avatar_changed") is True and t.get("avatar_url") == put[1]["avatar_url"],
          "dave changes his avatar after the report: avatar_changed true, the current address", (brief(put), t))
    r = call("GET", "/v1/admin/reports/99999999", cookie=CK["admin"])
    check(179, 5, problem(r, 404, "not-found"), "report 99999999: 404", brief(r))


def resolve(rid, action, origin=True):
    return call("POST", f"/v1/admin/reports/{rid}/resolve", cookie=CK["admin"], origin=origin, body={"action": action})


def tc180():
    did, _ = DAVE
    td = DAVE_APP[0]
    url = call("GET", "/v1/me", token=td)[1]["account"]["avatar_url"]
    r = resolve(R_DAVE, "remove_avatar")
    rep = r[1].get("report", {}) if isinstance(r[1], dict) else {}
    gone = call("GET", url)
    me = call("GET", "/v1/me", token=td)
    again = put_avatar("/v1/me/avatar", f"{PIC}/p179.png", "image/png", token=td)
    check(180, 1, r[0] == 200 and rep.get("status") == "actioned" and rep.get("action") == "remove_avatar"
          and "subscription_canceled" not in r[1] and problem(gone, 404, "not-found")
          and "avatar_url" not in me[1]["account"] and again[0] == 200,
          "resolve dave's report with remove_avatar: actioned; his avatar 404 and gone from /v1/me; he can set one again",
          (brief(r), brief(gone), brief(again)))
    r = resolve(R_DAVE, "dismiss")
    check(180, 2, problem(r, 409, "report-closed"), "resolving it again: 409 report-closed", brief(r))
    target = next(x for x in call("GET", "/v1/admin/reports", cookie=CK["admin"])[1]["reports"] if x["target"]["id"] == E[0])
    res = [None, None]

    def go(i):
        res[i] = resolve(target["id"], "dismiss")
    th = [threading.Thread(target=go, args=(i,)) for i in (0, 1)]
    [t.start() for t in th]
    [t.join() for t in th]
    check(180, 3, sorted(x[0] for x in res) == [200, 409] and any(problem(x, 409, "report-closed") for x in res),
          "two dismissals of the report on e1 at once: one 200, one 409 report-closed", [brief(x) for x in res])
    r = resolve(R_BOB_G, "delete")
    check(180, 4, problem(r, 400, "invalid-request"), "action delete: 400", brief(r))
    tb_ = TB
    rep = call("POST", "/v1/reports", token=tb_, body={"account_id": ADMIN[0], "reason": "other"})
    rid = rep[1]["report"]["id"]
    r = resolve(rid, "ban")
    v = report_of(rid)
    m = call("GET", "/v1/me", cookie=CK["admin"])
    d = resolve(rid, "dismiss")
    check(180, 5, rep[0] == 201 and problem(r, 403, "forbidden") and v[1]["report"]["status"] == "open"
          and m[0] == 200 and d[0] == 200,
          "bob reports the admin; resolving it with ban: 403 forbidden, still open, admin signed in; dismissing works",
          (brief(r), v[1]["report"]["status"], brief(d)))


def tc181(g177):
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    did, dt = DAVE
    # Before: carol has an avatar, a push token, a group she owns with alice,
    # a pending request to dave; each of alice and carol wrote the other.
    small = f"{PIC}/p181.png"
    open(small, "wb").write(png(64, 64, lambda x, y: GREEN))
    put_avatar("/v1/me/avatar", small, "image/png", token=TC)
    call("PUT", "/v1/app/push", token=TC, body={"push_token": "ExponentPushToken[t181-carol]"})
    imbo("carol", "group", "create", "g181")
    g181 = group_id("g181", "carol")
    imbo("carol", "group", "invite", f"#{g181}", str(a))
    imbo("alice", "group", "join", f"#{g181}")
    call("POST", "/v1/contacts/requests", token=DEV["carol"], body={"account_id": did, "note": "T181 hi dave"})
    imbo("alice", "send", "carol", "T181 alice waiting")
    imbo("carol", "send", "alice", "T181 carol waiting")
    say(f"   carol owns g181 #{g181} with alice; carol asked dave; alice and carol wrote each other")
    r = resolve(R_CAROL, "ban")
    rep = r[1].get("report", {}) if isinstance(r[1], dict) else {}
    check(181, 1, r[0] == 200 and rep.get("status") == "actioned" and rep.get("action") == "ban"
          and rep.get("target", {}).get("banned") is True and r[1].get("subscription_canceled") is True,
          "admin resolves alice's report on carol with ban: actioned, target banned, subscription_canceled true (no Waffo)",
          brief(r))
    time.sleep(1.5)
    m1 = call("GET", "/v1/me", token=TC)
    m2 = call("GET", "/v1/me", token=DEV["carol"])
    closed = [line for line in open(f"{L}/carol/daemon.log", errors="replace") if "server closed the connection" in line]
    code, out = imbo("carol", "inbox")
    check(181, 2, problem(m1, 401, "unauthorized") and problem(m2, 401, "unauthorized")
          and any("code=4401" in x for x in closed) and code != 0 and "imbo login" in out,
          "carol's App and computer get 401; her daemon was closed with 4401; her imbo says to sign in",
          (brief(m1), brief(m2), closed[-1:], out[-200:]))
    _, ia = imbo("alice", "inbox")
    check(181, 3, "T181 carol waiting" not in ia, "alice's inbox lacks T181 carol waiting", ia[-300:])
    other = report_of(next(x["id"] for x in call("GET", "/v1/admin/reports?status=all", cookie=CK["admin"])[1]["reports"]
                           if x["reporter"]["id"] == b and x["target"]["id"] == c))
    check(181, 4, other[1]["report"]["status"] == "actioned" and other[1]["report"]["action"] == "ban",
          "bob's open report on carol is closed too: actioned, ban", other[1]["report"]["status"])
    _, reqs, _ = call("GET", "/v1/requests", token=dt)
    pend = [x for x in reqs["requests"] if x["from"]["id"] == c and x["status"] == "pending"]
    check(181, 5, pend == [], "dave no longer has carol's pending request", reqs["requests"])
    rc = call("POST", "/v1/auth/code", body={"email": "carol@imbo.test"}, headers={"x-forwarded-for": "10.18.0.1"})
    check(181, 6, problem(rc, 403, "account-banned") and REPORT_EMAIL in rc[1]["detail"],
          "asking for a sign-in code for carol: 403 account-banned, the detail names reports@imbo.test", brief(rc))
    c_in = contact(TA, c)
    code, out = imbo("alice", "send", "carol", "T181 after ban")
    r = call("POST", "/v1/messages", token=TA, body={"to": str(c), "body": "T181 app", "client_msg_id": "t181-a"})
    check(181, 7, c_in and c_in.get("active") is False and "avatar_url" not in c_in and code != 0
          and "inactive" in out and problem(r, 403, "recipient-inactive"),
          "alice sees carol inactive, without avatar; writing her fails: 403 recipient-inactive",
          (c_in, out[-200:], brief(r)))
    n = push_count()
    s = call("POST", "/v1/messages", token=DEV["alice"], body={"to": f"#{g177}", "body": "T181 group", "client_msg_id": "t181-g"})
    _, groups, _ = call("GET", "/v1/groups", token=TA)
    gv = next(x for x in groups["groups"] if x["id"] == g177)
    cm = next((m for m in gv["members"] if m["id"] == c), {})
    time.sleep(2)
    check(181, 8, s[0] == 201 and s[1].get("inactive_members") == 1 and cm.get("active") is False
          and not pushes_for(c, n),
          "alice writes g177: 201 with inactive_members 1; carol shows inactive in g177; nothing pushed to carol",
          (brief(s), cm))
    acc = call("GET", f"/v1/admin/accounts?q={c}", cookie=CK["admin"])
    it = acc[1]["accounts"][0] if acc[0] == 200 and acc[1]["accounts"] else {}
    check(181, 9, it.get("banned_at") and "avatar_url" not in it,
          "the admin's account list shows carol with banned_at, without avatar", it)
    return g181


def tc182():
    a, b = ID["alice"], ID["bob"]
    adm = ADMIN[0]
    lst = call("GET", "/v1/admin/accounts", cookie=CK["admin"])
    accs = lst[1]["accounts"]
    me = next((x for x in accs if x["id"] == adm), {})
    al = next((x for x in accs if x["id"] == a), {})
    e8 = next((x for x in accs if x["id"] == E[7]), {})
    times = [x["created_at"] for x in accs]
    check(182, 1, lst[0] == 200 and lst[1]["total"] == len(accs) and times == sorted(times, reverse=True)
          and me.get("admin") is True and al.get("admin") is False
          and {"email", "created_at", "access"} <= set(al),
          "admin lists accounts: newest first, each with email, created_at, access; the admin's own marked admin",
          (lst[1]["total"], me, al))
    check(182, 2, al.get("last_active_at") and "last_active_at" not in e8,
          "alice has last_active_at; e8, never connected or sending, has none", (al.get("last_active_at"), e8))
    q1 = call("GET", f"/v1/admin/accounts?q={b}", cookie=CK["admin"])
    q2 = call("GET", "/v1/admin/accounts?q=ALICE@IMBO", cookie=CK["admin"])
    q3 = call("GET", "/v1/admin/accounts?q=nobody-here", cookie=CK["admin"])
    check(182, 3, [x["id"] for x in q1[1]["accounts"]] == [b] and [x["id"] for x in q2[1]["accounts"]] == [a]
          and q3[1]["total"] == 0,
          "q=<bob's ID>: only bob; q=ALICE@IMBO: only alice; q=nobody-here: none",
          (q1[1]["total"], q2[1]["total"], q3[1]["total"]))
    t2 = E_APP[1]
    r1 = call("POST", f"/v1/admin/accounts/{E[1]}/ban", cookie=CK["admin"], origin=True)
    r2 = call("POST", f"/v1/admin/accounts/{E[1]}/ban", cookie=CK["admin"], origin=True)
    check(182, 4, r1[0] == 200 and r1[1]["account"].get("banned_at") and r1[1]["subscription_canceled"] is True
          and r2[0] == 200 and r2[1]["account"]["banned_at"] == r1[1]["account"]["banned_at"],
          "ban e2 from the account list: 200 with banned_at; again: 200, same banned_at", (brief(r1), brief(r2)))
    rs = [call("POST", f"/v1/admin/accounts/{adm}/ban", cookie=CK["admin"], origin=True),
          call("POST", "/v1/admin/accounts/9999999/ban", cookie=CK["admin"], origin=True),
          call("POST", f"/v1/admin/accounts/{E[2]}/ban", cookie=CK["admin"])]
    check(182, 5, problem(rs[0], 403, "forbidden") and problem(rs[1], 404, "account-not-found")
          and problem(rs[2], 403, "forbidden"),
          "banning the admin: 403; account 9999999: 404 account-not-found; without the web Origin: 403",
          [brief(x) for x in rs])
    old = call("GET", "/v1/me", token=t2)
    u1 = call("DELETE", f"/v1/admin/accounts/{E[1]}/ban", cookie=CK["admin"], origin=True)
    u2 = call("DELETE", f"/v1/admin/accounts/{E[1]}/ban", cookie=CK["admin"], origin=True)
    back = app_login("e2@imbo.test")
    still = call("GET", "/v1/me", token=t2)
    check(182, 6, problem(old, 401, "unauthorized") and u1[0] == 200 and "banned_at" not in u1[1]["account"]
          and u2[0] == 200 and back[0] == 200 and back[1]["new_account"] is False
          and back[1]["account"]["id"] == E[1] and problem(still, 401, "unauthorized"),
          "e2's App got 401 while banned; unban: 200, again 200; e2 signs in again as the same account; the old token stays dead",
          (brief(old), brief(u1), brief(u2), brief(back), brief(still)))


def stop_server():
    pid = int(open(f"{L}/server.pid").read())
    os.kill(pid, 15)
    while True:
        try:
            os.kill(pid, 0)
            time.sleep(0.2)
        except ProcessLookupError:
            return


def start_server(extra_env=None, drop=()):
    env = {k: v for k, v in os.environ.items() if k not in drop}
    env.update(extra_env or {})
    subprocess.run(["sh", f"{HERE}/local-env.sh", "server"], env=env, capture_output=True, text=True)


def server_cmd(*args):
    p = subprocess.run([S, *args], env={**os.environ, "IMBO_DATA_DIR": f"{L}/server", "IMBO_MAIL_LOG_ONLY": "1"},
                       capture_output=True, text=True, timeout=120)
    lines = [x for x in (p.stdout + p.stderr).splitlines()
             if "INFO" not in x and "WARN" not in x and " LOG: " not in x]
    return p.returncode, "\n".join(lines)


def tc183():
    did, dt = DAVE
    stop_server()
    c1, o1 = server_cmd("account", "ban", str(did))
    c2, o2 = server_cmd("account", "ban", str(did))
    c3, o3 = server_cmd("account", "ban", str(ADMIN[0]))
    c4, o4 = server_cmd("account", "ban", "9999999")
    say(f"   server stopped; outputs: {o1!r} / {o2!r} / {o3!r} / {o4!r}")
    check(183, 1, c1 == 0 and f"Banned dave ({did}): 1 computer(s) signed out" in o1,
          "imbo-server account ban <dave>: exit 0, Banned dave, 1 computer signed out", (c1, o1))
    check(183, 2, c2 == 0 and f"dave ({did}) was already banned." in o2, "again: was already banned", (c2, o2))
    check(183, 3, c3 != 0 and "admin" in o3 and c4 != 0 and "no account 9999999" in o4,
          "the admin's ID: refused; 9999999: no account", (c3, o3, c4, o4))
    start_server()
    r1 = call("GET", "/v1/me", token=dt)
    r2 = call("POST", "/v1/auth/code", body={"email": "dave@imbo.test"}, headers={"x-forwarded-for": "10.18.0.2"})
    check(183, 4, problem(r1, 401, "unauthorized") and problem(r2, 403, "account-banned"),
          "server started again: dave's computer token 401; a code for dave: 403 account-banned", (brief(r1), brief(r2)))
    stop_server()
    c5, o5 = server_cmd("account", "unban", str(did))
    start_server()
    back = app_login("dave@imbo.test")
    check(183, 5, c5 == 0 and f"Unbanned {did}" in o5 and back[0] == 200 and back[1]["account"]["id"] == did,
          "account unban <dave>: Unbanned; dave signs in to the App again", (c5, o5, brief(back)))
    DAVE_APP[0] = back[1]["token"] if back[0] == 200 else None


def tc184(g181):
    a, b, c = ID["alice"], ID["bob"], ID["carol"]
    r1 = call("DELETE", f"/v1/admin/accounts/{b}", cookie=CK["admin"], origin=True, body={"confirm": "DELETE"})
    r2 = call("DELETE", f"/v1/admin/accounts/{c}", cookie=CK["admin"], origin=True, body={"confirm": "delete"})
    check(184, 1, problem(r1, 400, "invalid-request") and problem(r2, 400, "invalid-request"),
          "deleting bob, not banned: 400; carol with confirm delete: 400", (brief(r1), brief(r2)))
    r = call("DELETE", f"/v1/admin/accounts/{c}", cookie=CK["admin"], origin=True, body={"confirm": "DELETE"})
    check(184, 2, r[0] == 204, "deleting banned carol with DELETE: 204", brief(r))
    _, ia = imbo("alice", "inbox")
    notice = f"Group g181 (#{g181}) was dissolved because its owner's account was deleted."
    check(184, 3, notice in ia, "alice's inbox has the dissolve notice for carol's g181", ia[-400:])
    q = call("GET", f"/v1/admin/accounts?q={c}", cookie=CK["admin"])
    v = report_of(R_CAROL)
    check(184, 4, q[1]["total"] == 0 and v[1]["report"]["target"]["name"] == "Deleted account"
          and len(v[1]["report"]["messages"]) == 20,
          "carol is gone from the account list; the report on her stays, named Deleted account, with its 20 messages",
          (q[1]["total"], v[1]["report"]["target"]))
    again = call("DELETE", f"/v1/admin/accounts/{c}", cookie=CK["admin"], origin=True, body={"confirm": "DELETE"})
    new = app_login("carol@imbo.test", "carol2")
    check(184, 5, problem(again, 404, "account-not-found") and new[0] == 200 and new[1]["new_account"] is True
          and new[1]["account"]["id"] != c,
          "deleting her again: 404 account-not-found; carol@imbo.test signs up again as a new account", (brief(again), brief(new)))


def tc185():
    imbo("alice", "group", "create", "g185")
    gid = group_id("g185", "alice")
    photo = jpeg("p185", 200, 200, lambda x, y: BLUE)
    _, out = imbo("alice", "group", "avatar", f"#{gid}", photo)
    url = re.search(r"(http\S+\.jpg)", out)[1]
    r = call("DELETE", f"/v1/admin/avatars/{avatar_id(url)}", cookie=CK["admin"], origin=True)
    gone = call("GET", url)
    _, groups, _ = call("GET", "/v1/groups", token=TA)
    gv = next(x for x in groups["groups"] if x["id"] == gid)
    code, out = imbo("alice", "group", "avatar", f"#{gid}", photo)
    url2 = re.search(r"(http\S+\.jpg)", out)[1] if code == 0 else ""
    check(185, 1, r[0] == 204 and problem(gone, 404, "not-found") and "avatar_url" not in gv and code == 0,
          "admin deletes g185's picture: 204; the address 404; g185 has none; alice can set one again", (brief(r), gv, out))
    r1 = call("DELETE", f"/v1/admin/avatars/{avatar_id(url)}", cookie=CK["admin"], origin=True)
    r2 = call("DELETE", "/v1/admin/avatars/not-a-uuid", cookie=CK["admin"], origin=True)
    check(185, 2, problem(r1, 404, "not-found") and problem(r2, 404, "not-found"),
          "the same ID again, and not-a-uuid: 404 not-found", (brief(r1), brief(r2)))
    r = call("DELETE", f"/v1/admin/avatars/{avatar_id(url2)}", cookie=CK["bob"], origin=True)
    still = call("GET", url2)
    check(185, 3, problem(r, 404, "not-found") and still[0] == 200,
          "bob's web sign-in tries it: 404, the picture stays", (brief(r), still[0]))


def tc186():
    s = call("GET", "/v1/admin/stats", cookie=CK["admin"])
    acc = call("GET", "/v1/admin/accounts", cookie=CK["admin"])
    st = s[1]
    days = st.get("days", [])
    today = time.strftime("%Y-%m-%d", time.gmtime(time.time() + 8 * 3600))
    dates = [d["date"] for d in days]
    check(186, 1, s[0] == 200 and st["accounts"] == acc[1]["total"] and st["paid"] == 0 and st["messages_today"] >= 1
          and len(days) == 30 and dates[-1] == today and dates == sorted(dates)
          and days[-1]["messages"] == st["messages_today"] and days[-1]["accounts"] == st["accounts"]
          and days[-1]["active_7d"] == st["active_7d"],
          "stats: accounts equals the account list's total, paid 0, 30 days ending today in Beijing, today's row matches",
          {k: st.get(k) for k in ("accounts", "active_7d", "paid", "messages_today")} | {"last": days[-1:] if days else None})
    call("POST", "/v1/messages", token=DEV["alice"], body={"to": str(ID["bob"]), "body": "T186 one more", "client_msg_id": "t186"})
    s2 = call("GET", "/v1/admin/stats", cookie=CK["admin"])
    check(186, 2, s2[1]["messages_today"] == st["messages_today"] + 1, "one more message: messages_today goes up by 1",
          (st["messages_today"], s2[1]["messages_today"]))


def tc187():
    did, _ = DAVE
    td = DAVE_APP[0]
    put = put_avatar("/v1/me/avatar", f"{PIC}/p179.png", "image/png", token=td)
    url = put[1].get("avatar_url", "") if isinstance(put[1], dict) else ""
    blk = call("POST", "/v1/blocks", token=td, body={"account_id": ID["alice"]})
    rep = call("POST", "/v1/reports", token=td, body={"account_id": E[3], "reason": "spam"})
    rid = rep[1]["report"]["id"] if rep[0] == 201 else None
    bl_before = [x["account"]["id"] for x in call("GET", "/v1/blocks", token=TB)[1]["blocks"]]
    d = call("DELETE", "/v1/account", token=td, body={"confirm": "DELETE"})
    check(187, 1, put[0] == 200 and blk[0] == 201 and rep[0] == 201 and did in bl_before and d[0] == 204,
          "dave (blocked by bob) sets an avatar, blocks alice, reports e4, then deletes his account from the App: 204",
          (brief(put), brief(blk), brief(rep), bl_before, brief(d)))
    gone = call("GET", url)
    bl_after = [x["account"]["id"] for x in call("GET", "/v1/blocks", token=TB)[1]["blocks"]]
    v = report_of(rid)
    check(187, 2, problem(gone, 404, "not-found") and did not in bl_after
          and v[0] == 200 and v[1]["report"]["reporter"]["name"] == "Deleted account",
          "his avatar 404; bob's block list no longer has him; his report stays, from Deleted account",
          (brief(gone), bl_after, brief(v)))


def tc188():
    start_server(drop=("IMBO_REPORT_EMAIL",))
    r = call("POST", "/v1/reports", token=TB, body={"account_id": E[4], "reason": "other"})
    time.sleep(1)
    rid = r[1]["report"]["id"] if r[0] == 201 else None
    warn = [x for x in log().splitlines() if f"report_id={rid}" in x and "report mail not sent" in x]
    listed = any(x["id"] == rid for x in call("GET", "/v1/admin/reports", cookie=CK["admin"])[1]["reports"])
    check(188, 1, r[0] == 201 and len(warn) == 1 and "IMBO_REPORT_EMAIL is not set" in warn[0] and listed,
          "server without IMBO_REPORT_EMAIL: bob's report on e5 is 201, the log says not sent, the admin lists it",
          (brief(r), warn))
    start_server(extra_env={"IMBO_REPORT_EMAIL": "not-an-address"})
    r = call("POST", "/v1/reports", token=TB, body={"account_id": E[5], "reason": "other"})
    time.sleep(1)
    rid = r[1]["report"]["id"] if r[0] == 201 else None
    warn = [x for x in log().splitlines() if f"report_id={rid}" in x and "report mail not sent" in x]
    listed = any(x["id"] == rid for x in call("GET", "/v1/admin/reports", cookie=CK["admin"])[1]["reports"])
    check(188, 2, r[0] == 201 and len(warn) == 1 and listed,
          "with IMBO_REPORT_EMAIL=not-an-address the mail fails: the report on e6 is 201, logged, listed", (brief(r), warn))
    start_server()


def png_claiming(side, bit_depth):
    """A PNG that only claims its size: header, an empty image stream, end."""
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, bit_depth, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"")) + chunk(b"IEND", b""))


def tc191():
    t = app_login("t191@imbo.test", "t191")[1]["token"]
    huge = png_claiming(8192, 16)
    r1 = put_avatar("/v1/me/avatar", huge, "image/png", token=t)
    check(191, 1, problem(r1, 422, "image-invalid") and "too many pixels" in r1[1].get("detail", ""),
          f"a {len(huge)}-byte PNG claiming 8192x8192 at 16 bits (512 MiB of pixels): 422 image-invalid, too many pixels",
          brief(r1))
    r2 = put_avatar("/v1/me/avatar", png_claiming(2048, 8), "image/png", token=t)
    check(191, 2, problem(r2, 422, "image-invalid") and "Only JPEG and PNG" in r2[1].get("detail", ""),
          "the same claiming 2048x2048 at 8 bits (16 MiB): passes the size check, then 422 as it can't be decoded",
          brief(r2))
    photo = jpeg("p191", 600, 600, lambda x, y: BLUE)
    got = [None] * 3

    def put(i):
        got[i] = put_avatar("/v1/me/avatar", photo, "image/jpeg", token=t)
    threads = [threading.Thread(target=put, args=(i,)) for i in range(3)]
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    urls = {g[1].get("avatar_url") for g in got if g and g[0] == 200}
    me = call("GET", "/v1/me", token=t)[1]["account"].get("avatar_url")
    check(191, 3, all(g and g[0] == 200 for g in got) and len(urls) == 3 and me in urls
          and call("GET", me)[0] == 200,
          "three 600x600 JPEGs at once: all 200 with three addresses; /v1/me has one of them and it serves",
          ([brief(g) for g in got], me))


def tc192():
    # Requests are handled from a computer (DESIGN §10, API-010/011); reports from the App.
    rid, rt = computer_login("r192@imbo.test", "r192")
    xid, xt = computer_login("x192@imbo.test", "x192")
    ra = app_login("r192@imbo.test")[1]["token"]

    def ask(note):
        return call("POST", "/v1/contacts/requests", token=xt, body={"account_id": rid, "note": note})[1]["request"]["id"]
    call("POST", f"/v1/requests/{ask('T192 old note')}/decline", token=rt)
    call("POST", f"/v1/requests/{ask('T192 accepted note')}/accept", token=rt)
    for i in range(1, 26):
        call("POST", "/v1/messages", token=xt,
             body={"to": str(rid), "body": f"T192 m{i:02}", "client_msg_id": f"t192-m{i}"})
    call("DELETE", f"/v1/contacts/{rid}", token=xt)
    call("POST", "/v1/contacts/requests", token=xt, body={"account_id": rid, "note": "T192 pending note"})
    say(f"   x192 {xid}: a declined and an accepted request, 25 messages, contact removed, a pending request")
    r = call("POST", "/v1/reports", token=ra, body={"account_id": xid, "reason": "harassment"})
    check(192, 1, r[0] == 201, "r192's App reports x192: 201", brief(r))
    v = report_of(r[1]["report"]["id"]) if r[0] == 201 else (0, {}, {})
    msgs = v[1].get("report", {}).get("messages", []) if isinstance(v[1], dict) else []
    seen = [(m["kind"], m["body"]) for m in msgs]
    check(192, 2, len(msgs) == 20 and seen[0] == ("message", "T192 m07") and seen[-1] == ("request_note", "T192 pending note")
          and all(k == "message" for k, _ in seen[:-1]) and not any("old note" in b or "accepted note" in b for _, b in seen),
          "20 together: m07 to m25, then the pending request's note; the declined and accepted notes are not there",
          seen[:2] + seen[-2:])


def review_code_request(email):
    """API-001 for an address whose code isn't logged: returns the response."""
    IP[0] += 1
    return call("POST", "/v1/auth/code", body={"email": email},
                headers={"x-forwarded-for": f"10.19.{IP[0] // 250}.{IP[0] % 250 + 1}"})


def mailed(email):
    return len(re.findall(rf"mail-log-only code=\d{{6}} email={re.escape(email)}$", log(), re.M))


def tc190():
    review = "review@imbo.test"
    fixed = f"{int.from_bytes(os.urandom(3), 'big') % 1_000_000:06}"
    wrong = "000000" if fixed != "000000" else "111111"
    start_server(extra_env={"IMBO_REVIEW_EMAIL": review, "IMBO_REVIEW_CODE": fixed})
    say("   server restarted with IMBO_REVIEW_EMAIL=review@imbo.test and a fixed 6-digit IMBO_REVIEW_CODE")
    r = review_code_request("Review@Imbo.test")
    time.sleep(1)
    check(190, 1, r[0] == 202 and mailed(review) == 0,
          "asking a code for Review@Imbo.test: 202, and no mail line for review@imbo.test", (brief(r), mailed(review)))
    s = call("POST", "/v1/app/session", body={"email": review, "code": fixed, "name": "reviewer", "platform": "ios"})
    if s[0] == 200:
        SECRETS.append(s[1]["token"])
    me = call("GET", "/v1/me", token=s[1]["token"]) if s[0] == 200 else (0, {}, {})
    check(190, 2, s[0] == 200 and s[1]["new_account"] is True and me[1]["account"]["email"] == review,
          "the App signs in with the fixed code: 200, a new account review@imbo.test", (brief(s), me[1].get("account")))
    CODE_AT[review] = time.time()
    time.sleep(61)
    r = review_code_request(review)
    tries = [call("POST", "/v1/app/session", body={"email": review, "code": wrong, "platform": "ios"}) for _ in range(5)]
    after = call("POST", "/v1/app/session", body={"email": review, "code": fixed, "platform": "ios"})
    check(190, 3, r[0] == 202 and all(problem(x, 400, "code-invalid") for x in tries[:4])
          and problem(tries[4], 429, "code-locked") and after[0] in (400, 429) and "token" not in (after[1] or {}),
          "a new code, then 5 wrong tries: 400 four times, the 5th 429 code-locked; the fixed code now gets no token",
          (brief(r), [brief(x) for x in tries], brief(after)))
    other = "normal190@imbo.test"
    code, r = ask_code(other)
    bad = call("POST", "/v1/app/session", body={"email": other, "code": fixed, "name": "normal190", "platform": "ios"}) \
        if code != fixed else None
    good = call("POST", "/v1/app/session", body={"email": other, "code": code, "name": "normal190", "platform": "ios"})
    if good[0] == 200:
        SECRETS.append(good[1]["token"])
    check(190, 4, r[0] == 202 and mailed(other) == 1 and (bad is None or problem(bad, 400, "code-invalid"))
          and good[0] == 200,
          "normal190@imbo.test: its code is mailed (logged); the fixed code is wrong for it; its own code signs in",
          (brief(r), mailed(other), brief(bad) if bad else "same code", brief(good)))
    start_server(extra_env={"IMBO_REVIEW_EMAIL": review}, drop=("IMBO_REVIEW_CODE",))
    code, r = ask_code(review)
    check(190, 5, r[0] == 202 and code is not None and mailed(review) == 1,
          "server with IMBO_REVIEW_EMAIL only: a code for review@imbo.test is mailed as usual", (brief(r), mailed(review)))
    start_server(drop=("IMBO_REVIEW_EMAIL", "IMBO_REVIEW_CODE"))


def tc189():
    e = E[6]
    out = subprocess.run(["python3", f"{HERE}/waffo-event.py", "subscription.activated", "--order", "ORD_t189",
                          "--account", str(e), "--period-end", "+30d"],
                         env=os.environ, capture_output=True, text=True).stdout
    check(189, 1, "\n200" in out, "an active subscription ORD_t189 recorded for e7", out[-200:])
    before = log().count("waffo refused")
    r1 = call("POST", f"/v1/admin/accounts/{e}/ban", cookie=CK["admin"], origin=True)
    r2 = call("POST", f"/v1/admin/accounts/{e}/ban", cookie=CK["admin"], origin=True)
    refused = log().count("waffo refused") - before
    check(189, 2, r1[0] == 200 and r1[1]["subscription_canceled"] is False and r1[1]["account"].get("banned_at")
          and r2[0] == 200 and r2[1]["subscription_canceled"] is False and refused == 2,
          "ban e7, Waffo refuses the cancel: 200, banned, subscription_canceled false; again retries it: false; 2 refusals logged",
          (brief(r1), brief(r2), refused))
    t = E_APP[6]
    m = call("GET", "/v1/me", token=t)
    d = call("DELETE", f"/v1/admin/accounts/{e}", cookie=CK["admin"], origin=True, body={"confirm": "DELETE"})
    q = call("GET", f"/v1/admin/accounts?q={e}", cookie=CK["admin"])
    check(189, 3, problem(m, 401, "unauthorized") and problem(d, 502, "payment-provider-failed") and q[1]["total"] == 1,
          "e7's App is signed out all the same; deleting e7: 502 payment-provider-failed, the account stays",
          (brief(m), brief(d), q[1]["total"]))


# ---------- main ----------

def write(run, tc, header):
    path = os.path.join(os.environ.get("EVIDENCE_DIR", "docs/test-runs"), f"{run.lower()}-tc-{tc}.txt")
    with open(path, "w") as f:
        f.write(f"{run} TC-{tc} · {header}\n")
        f.write("\n".join(OUT) + "\n")
    OUT.clear()


if __name__ == "__main__":
    run = sys.argv[1]
    only = set(sys.argv[2:])
    header = os.environ.get("RUN_HEADER", "")
    ID = ids()
    DEV = {n: device_token(n) for n in ("alice", "bob", "carol")}
    E, E_APP, DAVE_APP = [], {}, [None]
    FRANK = DAVE = None
    R_CAROL = R_DAVE = R_BOB_G = None
    if only == {"189"}:
        state = json.load(open(f"{L}/safety-state.json"))
        E[:] = state["E"]
        E_APP = {int(k): v for k, v in state["E_APP"].items()}
        CK = {"admin": state["admin_cookie"]}
        SECRETS.extend([CK["admin"], *E_APP.values()])
        try:
            tc189()
        except Exception:
            check(189, "-", False, "crashed", traceback.format_exc()[-600:])
        write(run, 189, header)
        shutil.rmtree(PIC, ignore_errors=True)
        print(f"{FAILS} failed")
        sys.exit(1 if FAILS else 0)
    say(f"   accounts: alice {ID['alice']}, bob {ID['bob']}, carol {ID['carol']}")
    TA = app_login("alice@imbo.test")[1]["token"]
    TB = app_login("bob@imbo.test")[1]["token"]
    TC = app_login("carol@imbo.test")[1]["token"]
    CK = {"bob": web_login("bob@imbo.test"), "admin": web_login("admin@imbo.test", "admin")}
    ADMIN = [call("GET", "/v1/me", cookie=CK["admin"])[1]["account"]["id"]]
    say(f"   signed in: alice, bob, carol in the App; bob and admin ({ADMIN[0]}) on the web")
    state = {}
    steps = [("171", tc171), ("172", tc172), ("173", tc173), ("174", tc174), ("175", tc175),
             ("176", tc176), ("177", tc177), ("178", tc178), ("179", tc179), ("180", tc180),
             ("181", tc181), ("182", tc182), ("183", tc183), ("184", tc184), ("185", tc185),
             ("186", tc186), ("187", tc187), ("188", tc188), ("190", tc190), ("191", tc191),
             ("192", tc192)]
    for n, case in steps:
        if only and n not in only:
            continue
        try:
            if n == "177":
                res = case(state.get("173"))
            elif n == "181":
                res = case(state.get("177"))
            elif n == "184":
                res = case(state.get("181"))
            else:
                res = case()
            state[n] = res
        except Exception:  # report and go on with the next case
            check(n, "-", False, "crashed", traceback.format_exc()[-600:])
        write(run, n, header)
    # TC-189 runs later, after the server restarts with Waffo.
    json.dump({"E": E, "E_APP": E_APP, "admin_cookie": CK["admin"]}, open(f"{L}/safety-state.json", "w"))
    shutil.rmtree(PIC, ignore_errors=True)
    print(f"{FAILS} failed")
    sys.exit(1 if FAILS else 0)
