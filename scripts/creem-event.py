#!/usr/bin/env python3
"""Post one signed Creem webhook event to a local imbo-server (docs/TEST.md §3).

  creem-event.py subscription.paid --sub sub_t1 --account 12345678 --period-end +30d
  creem-event.py checkout.completed --sub sub_t1 --account 12345678
  creem-event.py refund.created --sub sub_t1
  creem-event.py subscription.update --sub sub_t1 --id evt_x --bad-signature

Signs the raw body with $IMBO_CREEM_WEBHOOK_SECRET the way Creem does (hex
HMAC-SHA256 in `creem-signature`); product IDs come from
$IMBO_CREEM_PRODUCT_MONTHLY / $IMBO_CREEM_PRODUCT_YEARLY. Prints the event ID,
then the HTTP status and response body.
"""
import argparse
import datetime as dt
import hashlib
import hmac
import json
import os
import re
import secrets
import sys
import urllib.error
import urllib.request

STATUS = {
    "checkout.completed": "active",
    "subscription.active": "active",
    "subscription.paid": "active",
    "subscription.update": "active",
    "subscription.scheduled_cancel": "scheduled_cancel",
    "subscription.canceled": "canceled",
    "subscription.past_due": "past_due",
    "subscription.expired": "expired",
    "subscription.paused": "paused",
}


def when(text, now):
    """ISO time, or relative to now: +30d, -2d, -1h."""
    m = re.fullmatch(r"([+-]\d+)([dh])", text)
    if m:
        n = int(m.group(1))
        return now + (dt.timedelta(days=n) if m.group(2) == "d" else dt.timedelta(hours=n))
    return dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def iso(t):
    return t.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("type")
    p.add_argument("--sub", required=True, help="Creem subscription ID")
    p.add_argument("--account", help="metadata.account_id")
    p.add_argument("--plan", choices=["monthly", "yearly"], default="monthly")
    p.add_argument("--product", help="product ID; overrides --plan")
    p.add_argument("--customer", default="cust_local_test")
    p.add_argument("--status", help="default follows the event type")
    p.add_argument("--period-end", help="current_period_end_date, ISO or +30d / -2d")
    p.add_argument("--updated", default="+0h", help="updated_at, ISO or relative; default now")
    p.add_argument("--id", help="event ID; default random")
    p.add_argument("--mode", default="test", help="test, or prod for a live-mode event")
    p.add_argument("--bad-signature", action="store_true")
    p.add_argument("--url", default="http://127.0.0.1:18080/v1/billing/creem/webhook")
    a = p.parse_args()

    secret = os.environ.get("IMBO_CREEM_WEBHOOK_SECRET")
    if not secret:
        sys.exit("IMBO_CREEM_WEBHOOK_SECRET is not set")
    product = a.product or os.environ.get(
        "IMBO_CREEM_PRODUCT_YEARLY" if a.plan == "yearly" else "IMBO_CREEM_PRODUCT_MONTHLY"
    )
    if not product:
        sys.exit("set IMBO_CREEM_PRODUCT_MONTHLY / IMBO_CREEM_PRODUCT_YEARLY, or pass --product")
    now = dt.datetime.now(dt.timezone.utc)
    event_id = a.id or "evt_" + secrets.token_hex(8)
    metadata = {"account_id": a.account} if a.account else {}

    sub = {
        "id": a.sub,
        "object": "subscription",
        "status": a.status or STATUS.get(a.type, "active"),
        "updated_at": iso(when(a.updated, now)),
        "mode": a.mode,
    }
    if a.period_end:
        sub["current_period_end_date"] = iso(when(a.period_end, now))
    if a.type == "checkout.completed":
        obj = {
            "id": "ch_" + secrets.token_hex(6),
            "object": "checkout",
            "mode": a.mode,
            "customer": {"id": a.customer, "object": "customer"},
            "product": {"id": product, "object": "product"},
            "metadata": metadata,
            "subscription": sub,
        }
    elif a.type.startswith("subscription."):
        obj = dict(sub, customer={"id": a.customer}, product={"id": product}, metadata=metadata)
    elif a.type in ("refund.created", "dispute.created"):
        obj = {
            "id": ("ref_" if a.type.startswith("refund") else "dis_") + secrets.token_hex(6),
            "object": a.type.split(".")[0],
            "mode": a.mode,
            "subscription": {"id": a.sub, "object": "subscription"},
        }
    else:
        obj = {"id": "obj_" + secrets.token_hex(6), "mode": a.mode}

    body = json.dumps(
        {"id": event_id, "eventType": a.type, "created_at": int(now.timestamp() * 1000), "object": obj},
        separators=(",", ":"),
    ).encode()
    key = secret.encode() if not a.bad_signature else b"not-the-secret"
    signature = hmac.new(key, body, hashlib.sha256).hexdigest()
    req = urllib.request.Request(
        a.url,
        data=body,
        headers={"content-type": "application/json", "creem-signature": signature},
    )
    print(event_id)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=10) as res:
            print(res.status, res.read().decode())
    except urllib.error.HTTPError as e:
        print(e.code, e.read().decode())


if __name__ == "__main__":
    main()
