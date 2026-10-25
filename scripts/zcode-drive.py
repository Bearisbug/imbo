#!/usr/bin/env python3
"""Drive a ZCode session through ZCode's app-server, without its window.

usage: zcode-drive.py <ZCode.app> <workdir> '<prompt>' [--then '<prompt>']… [--image PNG]…
                      [--watch SECONDS] [--model PROVIDER/MODEL] [--reasoning LEVEL]

Starts the app-server the desktop app talks to (NDJSON over stdio) in the
current HOME, opens a session in <workdir> in yolo mode with <prompt>, and
prints `session <id>`, then each finished turn: its texts and tool calls.
Each --then prompt is sent after the turn before it ends; --image pictures
are pasted into the first of them. After the last prompt's turn it keeps
watching --watch seconds (default 0) for turns the session starts by
itself, as when a background `imbo wait` ends. --reasoning is the model's
thinking level (default low); ZCode refuses a turn without one for models
that have levels, deepseek-flash among them. A window the app-server opens
is closed at once.
"""
import base64
import hashlib
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
import uuid


def out(*a):
    print(*a, flush=True)


class Server:
    def __init__(self, app, cwd):
        env = dict(os.environ, ELECTRON_RUN_AS_NODE="1")
        cmd = [f"{app}/Contents/MacOS/ZCode", f"{app}/Contents/Resources/glm/zcode.cjs", "app-server", "--stdio"]
        self.p = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL)
        self.next_id = 1
        self.pending = {}
        self.turn_ends = []
        self.ready = threading.Event()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if "method" in m and "id" in m:
                self._answer(m)
            elif "method" in m:
                params = m.get("params") or {}
                if m["method"] == "startup/storageState" and params.get("phase") == "ready":
                    self.ready.set()
                if m["method"] == "v4/telemetry/event" and params.get("kind") == "turn.terminal":
                    self.turn_ends.append(params.get("sessionId"))
            else:
                q = self.pending.pop(str(m.get("id")), None)
                if q:
                    q.put(m)

    def _answer(self, m):
        method = m["method"]
        if method == "session/requestRuntimePreferences":
            self.send({"id": m["id"], "result": {"nativeSearchEnhancementsEnabled": False, "memoryEnabled": False}})
        elif method == "interaction/requestPermission":
            self.send({"id": m["id"], "result": {"decision": "allow"}})
        else:
            self.send({"id": m["id"], "error": {"code": -32601, "message": "unsupported " + method}})

    def send(self, obj):
        self.p.stdin.write((json.dumps(obj) + "\n").encode())
        self.p.stdin.flush()

    def call(self, method, params, timeout=60):
        i = self.next_id
        self.next_id += 1
        q = queue.Queue()
        self.pending[str(i)] = q
        self.send({"id": i, "method": method, "params": params})
        r = q.get(timeout=timeout)
        if "error" in r:
            raise RuntimeError(json.dumps(r["error"])[:500])
        return r.get("result")

    def command(self, sid, typ, payload):
        return self.call("v4/command", {"commandId": str(uuid.uuid4()), "clientId": "zcode-drive", "sessionId": sid,
                                         "type": typ, "payload": payload, "issuedAt": int(time.time() * 1000)})

    def ends(self, sid):
        return sum(1 for s in self.turn_ends if s == sid)

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=15)
        except Exception:
            self.p.kill()


def close_windows(app, stop):
    """The app-server may launch the desktop app; end it."""
    while not stop.is_set():
        ps = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
        for line in ps.splitlines():
            if f"{app}/Contents/" in line and "zcode.cjs" not in line and "Contents/Resources/glm" not in line:
                try:
                    os.kill(int(line.split()[0]), signal.SIGTERM)
                    out("closed a ZCode window process")
                except OSError:
                    pass
        stop.wait(1)


def show_turn(server, sid, seen):
    r = server.call("session/messages", {"sessionId": sid, "limit": 300})
    ms = (r.get("messages") if isinstance(r, dict) else r) or []
    for m in ms[seen:]:
        role = (m.get("info") or {}).get("role")
        for p in m.get("parts", []):
            if p.get("type") == "text" and p.get("text"):
                out(f"  {role} text: {p['text'][:600]!r}")
            elif p.get("type") == "tool":
                st = p.get("state") or {}
                res = st.get("output") if st.get("output") is not None else st.get("error")
                out(f"  tool {p.get('tool')} {st.get('status')} input={json.dumps(st.get('input'), ensure_ascii=False)[:400]}"
                    f" output={str(res)[:600]!r}")
    return len(ms)


def attach(server, sid, path):
    """Upload a picture the way the desktop composer pastes one."""
    data = open(path, "rb").read()
    up = "up-" + uuid.uuid4().hex[:12]
    name = os.path.basename(path)
    server.call("v4/attachment/begin", {"connectionId": "zcode-drive", "uploadId": up, "sessionId": sid,
                                         "fileName": name, "mime": "image/png", "totalBytes": len(data),
                                         "totalChunks": 1, "checksum": "sha256:" + hashlib.sha256(data).hexdigest()})
    server.call("v4/attachment/chunk", {"connectionId": "zcode-drive", "sessionId": sid, "uploadId": up,
                                         "chunkIndex": 0, "dataBase64": base64.b64encode(data).decode()})
    c = server.call("v4/attachment/commit", {"connectionId": "zcode-drive", "sessionId": sid, "uploadId": up})
    return {"ref": c["ref"], "fileName": name, "mime": "image/png", "bytes": len(data)}


def main():
    args = sys.argv[1:]
    opts = {"--watch": ["0"], "--model": ["ds/deepseek-flash"], "--reasoning": ["low"], "--then": [], "--image": []}
    rest = []
    while args:
        a = args.pop(0)
        if a in opts:
            if a in ("--watch", "--model", "--reasoning"):
                opts[a] = []
            opts[a].append(args.pop(0))
        else:
            rest.append(a)
    if len(rest) != 3:
        sys.exit(__doc__)
    app, cwd, prompt = os.path.abspath(rest[0]), os.path.abspath(rest[1]), rest[2]
    watch = float(opts["--watch"][0])
    provider, _, model_id = opts["--model"][0].partition("/")
    selection = {"providerId": provider, "modelId": model_id, "options": {"reasoningLevel": opts["--reasoning"][0]}}
    prompts = opts["--then"]

    stop = threading.Event()
    threading.Thread(target=close_windows, args=(app, stop), daemon=True).start()
    server = Server(app, cwd)
    try:
        if not server.ready.wait(90):
            sys.exit("app-server not ready")
        r = server.command(None, "createSession", {"workspaceId": cwd, "config": {"mode": "yolo"}, "firstInput": {
            "text": prompt, "mode": "yolo", "modelSelection": selection}})
        sid = r["result"]["sessionId"]
        out("session", sid)
        seen, shown, sent = 0, 0, 0
        # Each prompt's turn gets 5 minutes.
        deadline = time.time() + 300
        while True:
            n = server.ends(sid)
            while shown < n:
                shown += 1
                out(f"turn {shown} ended at {time.strftime('%H:%M:%S')}")
                seen = show_turn(server, sid, seen)
                deadline = time.time() + (300 if sent < len(prompts) else watch)
                if sent < len(prompts) and shown == sent + 1:
                    refs = [attach(server, sid, p) for p in opts["--image"]] if sent == 0 else []
                    server.command(sid, "sendText", {"text": prompts[sent], "attachments": refs, "mode": "yolo",
                                                     "modelSelection": selection})
                    out(f"sent prompt {sent + 2}" + (f" with {len(refs)} pictures" if refs else ""))
                    sent += 1
            if time.time() >= deadline:
                if shown <= sent:
                    out("a prompt's turn didn't end within 5 minutes")
                break
            time.sleep(1)
    finally:
        server.close()
        stop.set()


if __name__ == "__main__":
    main()
