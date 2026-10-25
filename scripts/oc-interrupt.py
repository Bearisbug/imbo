#!/usr/bin/env python3
"""Stop an opencode session's turn the way the desktop app's run menu does.

usage: oc-interrupt.py <HOME> <session short ID>

Finds the session whose ID ends with the short ID in opencode's database
under <HOME>, then calls `POST /api/session/<ID>/interrupt` without
`resume` on the shared background service registered in
<HOME>/.local/state/opencode/service.json, and prints the status code.
Unlike Esc in the terminal UI, this leaves a message waiting in the
session's inbox where it is.
"""
import base64
import json
import os
import sqlite3
import sys
import urllib.error
import urllib.request


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    home, short = sys.argv[1:]
    db = sqlite3.connect(f"file:{home}/.local/share/opencode/opencode.db?mode=ro", uri=True)
    ids = [r[0] for r in db.execute("select id from session_v2") if r[0].lower().endswith(short.lower())]
    if len(ids) != 1:
        sys.exit(f"{len(ids)} opencode sessions end with {short}")
    svc = json.load(open(os.path.join(home, ".local/state/opencode/service.json")))
    req = urllib.request.Request(f"{svc['url']}/api/session/{ids[0]}/interrupt", method="POST", data=b"")
    if svc.get("password"):
        pair = base64.b64encode(f"opencode:{svc['password']}".encode()).decode()
        req.add_header("Authorization", "Basic " + pair)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            print(r.status)
    except urllib.error.HTTPError as e:
        print(e.code)
        sys.exit(1)


if __name__ == "__main__":
    main()
