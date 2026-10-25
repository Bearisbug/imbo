#!/usr/bin/env python3
"""Call one tool on `imbo mcp` over stdio and print the result.

usage: mcp-call.py <IMBO_HOME> <tool> '<json args>' [--session <kind>:<id>] [--hold SECONDS]

The binary is $IMBO, or `imbo` on PATH. --session makes the call act as that
session the way the real tool does: claude-code through
CLAUDE_CODE_SESSION_ID; codex and zcode through the request's _meta; cursor
and minimax-code through the imbo_session argument their hooks add (Cursor
also names every tool with an imbo_ prefix).
"""
import json
import os
import subprocess
import sys
import time


def main():
    args = sys.argv[1:]
    session = None
    hold = 0.0
    if "--session" in args:
        i = args.index("--session")
        session = args[i + 1]
        del args[i : i + 2]
    if "--hold" in args:
        i = args.index("--hold")
        hold = float(args[i + 1])
        del args[i : i + 2]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    home, tool = args[0], args[1]
    params = json.loads(args[2]) if len(args) > 2 else {}

    env = dict(os.environ)
    for k in ("CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CODEX_THREAD_ID",
              "IMBO_HOST", "CURSOR_CONVERSATION_ID", "CURSOR_AGENT", "ZCODE_SESSION_ID", "ZCODE_RUNTIME_ENV",
              "__MAVIS_MCODE_TOOLS_BROKER_ENDPOINT"):
        env.pop(k, None)
    env["IMBO_HOME"] = home
    meta = None
    client = "mcp-call"
    if session:
        kind, _, sid = session.partition(":")
        if kind == "claude-code":
            env["CLAUDE_CODE_SESSION_ID"] = sid
        elif kind == "codex":
            meta = {"x-codex-turn-metadata": json.dumps({"thread_id": sid})}
        elif kind == "zcode":
            env["IMBO_HOST"] = kind
            client = "zcode"
            meta = {"com.zcode/request-context": {"session_id": sid, "runtime_scope": "local"}}
        elif kind in ("cursor", "minimax-code"):
            env["IMBO_HOST"] = kind
            client = "Cursor" if kind == "cursor" else "mavis-local-runtime-mcp"
            params["imbo_session"] = sid
            if kind == "cursor":
                tool = "imbo_" + tool
        else:
            sys.exit(f"unknown session kind {kind}")

    exe = os.environ.get("IMBO", "imbo")
    p = subprocess.Popen([exe, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)

    def send(msg):
        p.stdin.write(json.dumps(msg) + "\n")
        p.stdin.flush()

    def recv(want_id):
        while True:
            line = p.stdout.readline()
            if not line:
                sys.exit("imbo mcp exited")
            msg = json.loads(line)
            if msg.get("id") == want_id:
                return msg

    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": client, "version": "0"}}})
    recv(1)
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    if session and session.startswith("claude-code:"):
        time.sleep(0.5)  # let the session register before acting as it
    call = {"name": tool, "arguments": params}
    if meta:
        call["_meta"] = meta
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": call})
    res = recv(2)
    if "error" in res:
        print(json.dumps(res["error"], ensure_ascii=False))
        code = 1
    else:
        r = res["result"]
        for c in r.get("content", []):
            if c.get("type") == "text":
                print(c["text"])
        code = 1 if r.get("isError") else 0
    if hold > 0:
        time.sleep(hold)
    p.stdin.close()
    p.terminate()
    p.wait(timeout=5)
    sys.exit(code)


if __name__ == "__main__":
    main()
