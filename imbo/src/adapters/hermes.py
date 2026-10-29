"""Imbo for Hermes Agent: written by `imbo setup`, removed by `imbo uninstall`.

Offers Imbo's tools to the agent, each call going to an `imbo mcp` process
that is told the session, and puts messages from other sessions into them
through a socket only Imbo's background daemon knows the token of. Sessions
run in Hermes's background gateway, so that is where this works; the copy
Hermes loads in its terminal client stays idle.
"""

import atexit
import json
import logging
import os
import secrets
import socket
import subprocess
import tempfile
import threading

IMBO = __IMBO__
ENV = __ENV__
TOOLS = __TOOLS__
NOTE = __NOTE__
# Where each gateway tells Imbo's daemon its socket.
GATEWAYS = __GATEWAYS__

log = logging.getLogger("imbo")


def route(session_id):
    return f"agent:main:local:dm:{session_id}"


class Inbox:
    """The socket, started by the first session seen, and the sessions that
    have a client: from their first turn until the client closes them."""

    def __init__(self, ctx):
        self.ctx = ctx
        self.lock = threading.Lock()
        self.open = set()
        self.started = False

    def opened(self, session_id):
        if not session_id:
            return
        with self.lock:
            self.open.add(session_id)
            if self.started:
                return
            self.started = True
        try:
            self.start()
        except Exception:
            log.exception("Imbo couldn't open its socket")

    def closed(self, session_id):
        with self.lock:
            self.open.discard(session_id)

    def start(self):
        token = secrets.token_hex(24)
        folder = tempfile.mkdtemp(prefix="imbo-hermes-")
        path = os.path.join(folder, "in.sock")
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(path)
        server.listen(8)
        os.makedirs(GATEWAYS, exist_ok=True)
        entry = os.path.join(GATEWAYS, f"{os.getpid()}.json")
        with open(entry + ".tmp", "w") as f:
            json.dump({"socket": path, "token": token}, f)
        os.replace(entry + ".tmp", entry)

        def cleanup():
            for p in (entry, path):
                try:
                    os.remove(p)
                except OSError:
                    pass
            try:
                os.rmdir(folder)
            except OSError:
                pass

        atexit.register(cleanup)
        threading.Thread(target=self.serve, args=(server, token), daemon=True).start()

    def serve(self, server, token):
        while True:
            conn, _ = server.accept()
            threading.Thread(target=self.answer, args=(conn, token), daemon=True).start()

    def answer(self, conn, token):
        with conn:
            conn.settimeout(5)
            buf = b""
            try:
                while b"\n" not in buf:
                    chunk = conn.recv(65536)
                    if not chunk:
                        return
                    buf += chunk
                reply = self.handle(json.loads(buf.split(b"\n", 1)[0]), token)
            except Exception as e:
                reply = {"ok": False, "error": str(e)}
            conn.sendall((json.dumps(reply) + "\n").encode())

    def handle(self, req, token):
        if req.get("token") != token:
            return {"ok": False, "error": "wrong token"}
        if req.get("op") == "sessions":
            with self.lock:
                return {"ok": True, "sessions": sorted(self.open)}
        if req.get("op") == "deliver":
            session_id = req.get("session_id", "")
            with self.lock:
                if session_id not in self.open:
                    return {"ok": False, "error": "the session is closed"}
            # Idle, the session starts a turn; busy, it takes the message
            # once this turn ends.
            if self.ctx.inject_message(req.get("text", ""), session_key=route(session_id)):
                return {"ok": True}
            return {"ok": False, "error": "Hermes didn't take the message; see its gateway log"}
        return {"ok": False, "error": "unknown request"}


def call(name, args, session_id):
    """One call of an Imbo tool, through a fresh `imbo mcp`."""
    env = dict(os.environ)
    env.update(ENV)
    proc = subprocess.Popen(
        [IMBO, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
        text=True,
    )

    def send(msg):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()

    def answer(n):
        for line in proc.stdout:
            msg = json.loads(line)
            if msg.get("id") == n:
                if "error" in msg:
                    raise RuntimeError(msg["error"].get("message", "imbo mcp failed"))
                return msg["result"]
        raise RuntimeError("imbo mcp exited")

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "hermes-imbo", "version": "1"}}})
        answer(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": name, "arguments": args, "_meta": {"imbo_session": session_id}}})
        result = answer(2)
    finally:
        proc.stdin.close()
        proc.wait()
    text = "\n".join(c.get("text", "") for c in result.get("content", []) if c.get("type") == "text")
    return f"Error: {text}" if result.get("isError") else text


def register(ctx):
    inbox = Inbox(ctx)

    def tool(name):
        def handler(args, session_id="", **_):
            inbox.opened(session_id)
            return call(name, args or {}, session_id)
        return handler

    for t in TOOLS:
        ctx.register_tool(name=t["name"], toolset="imbo", schema=t, handler=tool(t["name"]))

    ctx.register_hook("on_session_start", lambda session_id="", **_: inbox.opened(session_id))
    ctx.register_hook("pre_llm_call", lambda session_id="", **_: inbox.opened(session_id))
    ctx.register_hook("on_session_finalize", lambda session_id="", **_: inbox.closed(session_id))
    ctx.register_hook(
        "on_session_reset",
        lambda session_id="", old_session_id="", **_: inbox.closed(old_session_id or session_id),
    )
    ctx.register_hook("on_session_delete", lambda session_id="", **_: inbox.closed(session_id))
    # Without this, models take a message from another session for a prompt
    # injection and refuse it.
    if hasattr(ctx, "register_system_prompt_section"):
        ctx.register_system_prompt_section("imbo.messages", NOTE, max_chars=4000)
