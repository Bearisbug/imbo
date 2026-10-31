#!/usr/bin/env python3
"""TC-463 (docs/TEST.md): the live-update connection for the web app and
the App (API-079, FLOW-LIVE), against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/events-tests.py <RUN-ID>

Starts its own server and seed the way parity-tests.py does, writes its
lines to docs/test-runs/<run>-tc-463.txt (with RUN_HEADER, if set, on the
first line) and exits 1 when any step failed. Standard library only; about
a minute, most of it waiting for the server to close a signed-out
connection.
"""

import base64
import importlib.util
import json
import os
import socket
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, OUT, call, check, imbo = P.L, P.OUT, P.call, P.check, P.imbo
SID = "55555555-aaaa-bbbb-cccc-000000"
WAIT = 2


class Conn:
    """A WebSocket client for `/v1/events`, just enough to read the
    server's frames."""

    def __init__(self, **headers):
        self.sock = socket.create_connection(("127.0.0.1", 18080), timeout=5)
        key = base64.b64encode(os.urandom(16)).decode()
        lines = ["GET /v1/events HTTP/1.1", "Host: 127.0.0.1:18080", "Upgrade: websocket",
                 "Connection: Upgrade", f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13",
                 *(f"{k}: {v}" for k, v in headers.items())]
        self.sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode())
        self.buf, self.eof, self.close_code = b"", False, None
        while b"\r\n\r\n" not in self.buf and self._fill(time.time() + 5):
            pass
        head, _, self.buf = self.buf.partition(b"\r\n\r\n")
        self.status = int(head.split(b" ")[1]) if head else 0

    def _fill(self, deadline):
        left = deadline - time.time()
        if left <= 0 or self.eof:
            return False
        self.sock.settimeout(left)
        try:
            chunk = self.sock.recv(65536)
        except (socket.timeout, OSError):
            return False
        self.eof = not chunk
        self.buf += chunk
        return bool(chunk)

    def _frame(self):
        b = self.buf
        if len(b) < 2:
            return None
        n, i = b[1] & 0x7F, 2
        if n == 126:
            n, i = (struct.unpack(">H", b[2:4])[0], 4) if len(b) >= 4 else (None, 0)
        elif n == 127:
            n, i = (struct.unpack(">Q", b[2:10])[0], 10) if len(b) >= 10 else (None, 0)
        if n is None or len(b) < i + n:
            return None
        self.buf = b[i + n:]
        return b[0] & 0x0F, b[i:i + n]

    def areas(self, wait=WAIT):
        """Areas named in the frames that arrive within `wait` seconds."""
        got, deadline = [], time.time() + wait
        while True:
            f = self._frame()
            if f is None:
                if self.close_code or not self._fill(deadline):
                    return got
                continue
            op, data = f
            if op == 1:
                got.append(json.loads(data).get("area"))
            elif op == 8:
                self.close_code = struct.unpack(">H", data[:2])[0] if len(data) >= 2 else 1005
                return got

    def drain(self):
        self.areas(0.5)


def fake(sid, title):
    return imbo("alice", "dev", "fake-session", f"{SID}{sid}", "--title", title)


def tc463():
    P.up("alice:bob,alice:carol", IMBO_WEB_ORIGIN=P.ORIGIN)
    i = P.ids()
    cookie = P.web_cookie("alice@imbo.test")
    bob = P.app_token("bob@imbo.test")
    carol = P.app_token("carol@imbo.test")
    web = {"Cookie": cookie, "Origin": P.ORIGIN}

    c = Conn()
    check(463, 1, "不带登录连", c.status == 401, f"status={c.status}")
    c = Conn(Authorization=f"Bearer {P.computer_token(f'{L}/alice')}")
    check(463, 2, "电脑 token 连", c.status == 403, f"status={c.status}")
    c = Conn(Cookie=cookie, Origin="https://evil.example")
    check(463, 3, "别的 Origin 连", c.status == 403, f"status={c.status}")

    w = Conn(**web)
    b1, b2 = Conn(Authorization=f"Bearer {bob}"), Conn(Authorization=f"Bearer {bob}")
    cc = Conn(Authorization=f"Bearer {carol}")
    conns = [w, b1, b2, cc]
    st = [x.status for x in conns]
    check(463, 4, "连上 W、B1、B2、C", st == [101] * 4, f"status={st}")

    def step(n, what, action, want):
        for x in conns:
            x.drain()
        action()
        got = [x.areas() for x in conns]
        ok = all(cond(g) for cond, g in zip(want, got))
        check(463, n, what, ok, "W={} B1={} B2={} C={}".format(*got))

    has = lambda *a: lambda g: all(x in g for x in a)
    lacks = lambda *a: lambda g: not any(x in g for x in a)
    anyg = lambda g: True

    step(5, "登记会话 t463", lambda: fake("463001", "t463"),
         [has("agents"), has("contacts"), has("contacts"), has("contacts")])
    step(6, "同样再登记一次", lambda: fake("463001", "t463"),
         [lacks("agents"), lacks("contacts"), anyg, anyg])
    step(7, "bob 发 T463-1 给 alice", lambda: imbo("bob", "send", "alice", "T463-1"),
         [has("conversations"), has("conversations"), has("conversations"), lacks("conversations")])

    def invite():
        imbo("alice", "group", "create", "t463")
        imbo("alice", "group", "invite", "t463", str(i["carol"]))
    step(8, "建群 t463 并邀请 carol", invite,
         [has("groups", "requests"), lacks("groups"), anyg, has("requests")])
    step(9, "carol 入群", lambda: imbo("carol", "group", "join", "t463"),
         [has("groups", "conversations", "requests"), anyg, anyg,
          has("groups", "conversations", "requests")])

    fake("463002", "t463b")
    for x in conns:
        x.drain()
    imbo("alice", "group", "sessions", "t463", "--sessions", "463001,463002")
    got = cc.areas(WAIT + 1)
    check(463, 10, "一次改选两个会话", got.count("groups") == 1, f"C={got}")

    step(11, "carol 退群", lambda: imbo("carol", "group", "leave", "t463", "--yes"),
         [has("groups"), anyg, anyg, has("groups")])

    w.drain()
    s, _, _ = call("DELETE", "/v1/web/session", cookie=cookie)
    start = time.time()
    while w.close_code is None and not w.eof and time.time() - start < 35:
        w.areas(1)
    again = Conn(**web)
    check(463, 12, "退出登录后", s == 204 and w.close_code == 4401 and again.status == 401,
          f"logout={s} 关闭码={w.close_code} 用时={time.time() - start:.0f}s 再连={again.status}")


def main():
    run = sys.argv[1]
    head = os.environ.get("RUN_HEADER")
    root = os.path.dirname(os.path.dirname(HERE))
    OUT.clear()
    if head:
        OUT.append(head.replace("<TC>", "TC-463"))
    try:
        tc463()
    except Exception as e:  # a step the build can't take yet still leaves a record
        check(463, 0, "中断", False, f"{type(e).__name__}: {e}")
    with open(f"{root}/docs/test-runs/{run.lower()}-tc-463.txt", "w") as f:
        f.write("\n".join(OUT) + "\n")
    subprocess.run(["sh", P.ENV, "down"], capture_output=True)
    print(f"failed steps: {P.FAILS}")
    sys.exit(1 if P.FAILS else 0)


if __name__ == "__main__":
    main()
