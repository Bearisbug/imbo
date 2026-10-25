#!/usr/bin/env python3
"""Send one request on the daemon socket and print the reply line.

usage: local-op.py <IMBO_HOME> wait <tool> <session> [--pid N] [--no-confirm] [--timeout S]
       local-op.py <IMBO_HOME> listening|unlisten|hook-prompt <tool> <session>
       local-op.py <IMBO_HOME> register <tool> <session> <pid>

`wait` is a listener (API-LOCAL-010): it blocks until the daemon answers,
prints the reply, and confirms with `printed` unless --no-confirm, so the
daemon can tell a notice that reached the screen from one that didn't. It
exits 3 when --timeout passes first. The daemon must already be running.
"""
import json
import os
import socket
import sys


def main():
    args = sys.argv[1:]
    opts = {}
    for flag, takes in (("--pid", True), ("--timeout", True), ("--no-confirm", False)):
        if flag in args:
            i = args.index(flag)
            opts[flag] = args[i + 1] if takes else True
            del args[i : i + (2 if takes else 1)]
    if len(args) < 4:
        sys.exit(__doc__)
    home, op, tool, session = args[:4]
    req = {"tool": tool, "session_id": session}
    if op == "wait":
        req["op"] = "wait"
        if "--pid" in opts:
            req["pid"] = int(opts["--pid"])
    elif op in ("listening", "unlisten", "hook-prompt"):
        req["op"] = op.replace("-", "_")
    elif op == "register" and len(args) == 5:
        req.update(op="register", pid=int(args[4]), messaging_socket=None, messaging_token=None)
    else:
        sys.exit(__doc__)

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(os.path.join(home, "daemon.sock"))
    if "--timeout" in opts:
        s.settimeout(float(opts["--timeout"]))
    s.sendall((json.dumps(req) + "\n").encode())
    f = s.makefile("rb")
    try:
        line = f.readline()
    except socket.timeout:
        print("timeout")
        sys.exit(3)
    print(line.decode().rstrip("\n"))
    if op == "wait" and "--no-confirm" not in opts:
        reply = json.loads(line or b"{}")
        if reply.get("text"):
            s.sendall(b"printed\n")
            # Hold until the daemon has read the confirmation.
            s.shutdown(socket.SHUT_WR)
            f.read()
    s.close()


if __name__ == "__main__":
    main()
