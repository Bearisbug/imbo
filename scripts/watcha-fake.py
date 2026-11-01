#!/usr/bin/env python3
"""A fake Watcha for local tests (docs/TEST.md §3 支付宝时长包).

  watcha-fake.py serve                 answer POST /v1/entitlements/access on 18092
  watcha-fake.py pay <user> [--plan monthly|yearly] [--starts <ISO>]
                                       mark this user as having paid, as the
                                       Owner's simulated purchase would
  watcha-fake.py fail <400|500|slow|unknown|unavailable|off>
                                       how the next answers go wrong

State is $L/watcha/state.json, the same shape for every command. The server
asks this fake only when IMBO_WATCHA_API_URL points at it, which needs
IMBO_DEV=1; watcha-env.sh exports both.
"""

import argparse
import datetime as dt
import json
import os
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 18092
API_KEY = "sandbox-local-key"
ENTITLEMENT = "ent_local_timepack"
DAYS = {"monthly": 31, "yearly": 365}


def path(name):
    return os.path.join(os.environ.get("WATCHA_DIR", "."), name)


def load():
    try:
        with open(path("state.json")) as f:
            return json.load(f)
    except FileNotFoundError:
        return {"paid": {}, "fail": "off"}


def save(state):
    with open(path("state.json"), "w") as f:
        json.dump(state, f)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def answer(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        if not re.fullmatch(r"/v1/entitlements/access", self.path):
            self.answer(404, {"error": {"code": "not_found", "message": "No such path"}})
            return
        if self.headers.get("authorization") != f"Bearer {API_KEY}":
            self.answer(401, {"error": {"code": "invalid_api_key", "message": "Bad key"}})
            return
        length = int(self.headers.get("content-length") or 0)
        req = json.loads(self.rfile.read(length) or b"{}")
        state = load()
        fail = state.get("fail", "off")
        if fail == "slow":
            time.sleep(8)
        if fail == "400":
            self.answer(400, {"error": {"code": "invalid_request", "message": "Bad request"}})
            return
        if fail == "500":
            self.answer(500, {"error": {"code": "internal", "message": "Try later"}})
            return
        if fail == "unknown":
            self.answer(200, {"access": "something_else"})
            return
        if fail == "unavailable":
            self.answer(200, {"access": "unavailable",
                              "reason": {"code": "purchasing_suspended", "message": "Off the shelf"}})
            return
        if req.get("entitlement_id") != ENTITLEMENT:
            self.answer(404, {"error": {"code": "entitlement_not_found", "message": "No such entitlement"}})
            return
        paid = state["paid"].get(str(req.get("user_id")))
        if paid:
            self.answer(200, {"access": "granted",
                              "entitlement": {"type": "duration", **paid}})
            return
        # The real purchase link carries a one-time ticket.
        ticket = f"tkt_local_{req.get('user_id')}"
        body = {"access": "purchase_required",
                "entitlement": {"type": "duration"},
                "purchase": {"url": f"http://127.0.0.1:{PORT}/purchase?ticket={ticket}",
                             "qr_code_url": f"http://127.0.0.1:{PORT}/qr?ticket={ticket}"}}
        if req.get("return_url"):
            body["purchase"]["url"] += "&return_url=" + req["return_url"]
        self.answer(200, body)


def cmd_serve(_args):
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    with open(path("serve.pid"), "w") as f:
        f.write(str(os.getpid()))
    server.serve_forever()


def cmd_pay(args):
    starts = dt.datetime.fromisoformat(args.starts) if args.starts \
        else dt.datetime.now(dt.timezone.utc)
    if starts.tzinfo is None:
        starts = starts.replace(tzinfo=dt.timezone.utc)
    expires = starts + dt.timedelta(days=DAYS[args.plan])
    state = load()
    state["paid"][args.user] = {"starts_at": int(starts.timestamp()),
                                "expires_at": int(expires.timestamp())}
    save(state)
    print(f"{args.user}: {args.plan} {starts.isoformat()} → {expires.isoformat()}")


def cmd_fail(args):
    state = load()
    state["fail"] = args.how
    save(state)
    print(f"next answers: {args.how}")


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("serve")
    pay = sub.add_parser("pay")
    pay.add_argument("user")
    pay.add_argument("--plan", choices=sorted(DAYS), default="monthly")
    pay.add_argument("--starts")
    fail = sub.add_parser("fail")
    fail.add_argument("how", choices=["400", "500", "slow", "unknown", "unavailable", "off"])
    args = p.parse_args()
    {"serve": cmd_serve, "pay": cmd_pay, "fail": cmd_fail}[args.cmd](args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
