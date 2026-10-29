#!/usr/bin/env python3
"""TC-345 step 1 (docs/TEST.md): an upload past the day's 1 GB, sent whole
before its answer is read, as iOS and browsers send it, against the local
server.

    . crates/scripts/local-env.sh
    crates/scripts/local-env.sh up alice:bob
    python3 crates/scripts/upload-tests.py

Uploads from alice's computer sign-in until 2 MB of her day are left, then
sends 40 MB. Prints PASS or FAIL; exits 1 on FAIL.
"""

import hashlib
import http.client
import json
import os
import re
import sys

L = os.environ["L"]
MB = 1024 * 1024


def token(name):
    text = open(f"{L}/{name}/credentials").read()
    return re.search(r'^token = "(.*)"', text, re.M).group(1)


def upload(name, size):
    """Sends the whole body, then reads the answer: (status, parsed body), or
    (None, the error that cut the request off)."""
    body = os.urandom(1024) * (size // 1024)
    c = http.client.HTTPConnection("127.0.0.1", 18080, timeout=60)
    try:
        c.request("POST", "/v1/files", body=body, headers={
            "authorization": f"Bearer {token(name)}",
            "content-type": "application/octet-stream",
            "imbo-file-name": "t345.bin",
            "imbo-file-sha256": hashlib.sha256(body).hexdigest(),
        })
        r = c.getresponse()
        raw = r.read()
        return r.status, json.loads(raw) if raw else None
    except OSError as e:
        return None, f"{type(e).__name__}: {e}"
    finally:
        c.close()


left = 1024 * MB - 2 * MB
while left > 0:
    n = min(left, 100 * MB)
    status, body = upload("alice", n)
    if status != 201:
        sys.exit(f"filling the day: {status} {body}")
    left -= n

status, body = upload("alice", 40 * MB)
ok = (
    status == 429
    and isinstance(body, dict)
    and body.get("type") == "/errors/upload-quota-exceeded"
    and body.get("detail", "").startswith("You uploaded 1022.0 MB today and this file is 40.0 MB")
)
print(f"{'PASS' if ok else 'FAIL'} TC-345 #1 40 MB past the day's limit: {status} {body}", flush=True)
sys.exit(0 if ok else 1)
