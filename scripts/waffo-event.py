#!/usr/bin/env python3
"""Post one signed Waffo webhook event to a local imbo-server (docs/TEST.md §3).

  waffo-event.py subscription.activated --order ORD_t1 --account 12345678 --period-end +30d
  waffo-event.py subscription.canceling --order ORD_t1 --period-end +30d
  waffo-event.py refund.succeeded --order ORD_t1
  waffo-event.py subscription.renewed --order ORD_t1 --bad-signature

Signs `<t>.<body>` with $L/webhook-key.pem (made by billing-env.sh) the way
Waffo does: RSA-SHA256, base64, in `X-Waffo-Signature: t=<ms>,v1=<sig>`. The
server checks it against IMBO_WAFFO_WEBHOOK_PUBLIC_KEY. Prints the event ID,
then the HTTP status and response body.
"""
import argparse
import base64
import datetime as dt
import json
import os
import re
import secrets
import subprocess
import sys
import urllib.error
import urllib.request

STATUS = {
    "subscription.activated": "active",
    "subscription.renewed": "active",
    "subscription.recovered": "active",
    "subscription.uncanceled": "active",
    "subscription.past_due": "past_due",
    "subscription.canceling": "canceling",
    "subscription.canceled": "canceled",
}


def when(text, now):
    """ISO time, or relative to now: +30d, -2d, -1h, -46m."""
    m = re.fullmatch(r"([+-]\d+)([dhm])", text)
    if m:
        n = int(m.group(1))
        unit = {"d": "days", "h": "hours", "m": "minutes"}[m.group(2)]
        return now + dt.timedelta(**{unit: n})
    return dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def iso(t):
    return t.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def day(t):
    return t.astimezone(dt.timezone.utc).strftime("%Y-%m-%d")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("type")
    p.add_argument("--order", required=True, help="Waffo order ID, ORD_…")
    p.add_argument("--account", help="orderMetadata.account_id")
    p.add_argument("--identity", help="merchantProvidedBuyerIdentity")
    p.add_argument("--plan", default="monthly", help="billingPeriod")
    p.add_argument("--status", help="orderStatus; default follows the event type")
    p.add_argument("--period-end", help="currentPeriodEnd, a date, ISO or +30d / -2d")
    p.add_argument("--time", default="+0h", help="the event's timestamp, ISO or relative; default now")
    p.add_argument("--sign-time", default="+0m", help="t in the signature header; default now")
    p.add_argument("--id", help="eventId; default built the way Waffo builds it for this event type")
    p.add_argument("--mode", default="test", help="test, or prod for a live-mode event")
    p.add_argument("--bad-signature", action="store_true", help="change the body after signing")
    p.add_argument("--key", default=os.path.join(os.environ.get("L", "/tmp/imbo-t"), "webhook-key.pem"))
    p.add_argument("--url", default="http://127.0.0.1:18080/v1/billing/waffo/webhook")
    a = p.parse_args()

    if not os.path.exists(a.key):
        sys.exit(f"{a.key} is missing; source billing-env.sh first")
    now = dt.datetime.now(dt.timezone.utc)
    event_time = when(a.time, now)
    data = {
        "orderId": a.order,
        "buyerEmail": "buyer@imbo.test",
        "currency": "USD",
        "orderMetadata": {"account_id": a.account} if a.account else {},
        "productMetadata": {},
        "productName": f"Imbo {a.plan}",
    }
    if a.identity:
        data["merchantProvidedBuyerIdentity"] = a.identity
    if a.type in STATUS:
        data["orderStatus"] = a.status or STATUS[a.type]
        data["billingPeriod"] = a.plan
        if a.period_end:
            end = when(a.period_end, now)
            data["currentPeriodStart"] = day(now)
            data["currentPeriodEnd"] = day(end) if re.fullmatch(r"[+-]\d+[dhm]", a.period_end) else a.period_end
    elif a.type.startswith("refund."):
        data.update(paymentId="PAY_" + secrets.token_hex(8), refundStatus=a.type.split(".")[1],
                    refundCreatedAt=iso(now))

    # Waffo's eventId: the order ID for events that happen once per order, the
    # order ID plus the event time for ones that can repeat, the order ID plus
    # the period end for renewals, and the refund ID for refunds.
    if a.id:
        event_id = a.id
    elif a.type.startswith("refund."):
        event_id = "REF_" + secrets.token_hex(11)
    elif a.type in ("subscription.canceling", "subscription.uncanceled", "subscription.past_due",
                    "subscription.recovered"):
        event_id = f"{a.order}-{iso(event_time)}"
    elif a.type == "subscription.renewed":
        event_id = f"{a.order}-renewed-{data.get('currentPeriodEnd') or day(now)}"
    else:
        event_id = a.order

    body = json.dumps(
        {"id": event_id, "timestamp": iso(event_time), "eventType": a.type, "eventId": event_id,
         "storeId": os.environ.get("IMBO_WAFFO_STORE_ID", "STO_local"), "storeName": "Imbo",
         "mode": a.mode, "data": data},
        separators=(",", ":"),
    )
    t = str(int(when(a.sign_time, now).timestamp() * 1000))
    sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", a.key], input=f"{t}.{body}".encode(),
                         capture_output=True, check=True).stdout
    if a.bad_signature:
        body += " "
    req = urllib.request.Request(
        a.url,
        data=body.encode(),
        headers={"content-type": "application/json",
                 "x-waffo-signature": f"t={t},v1={base64.b64encode(sig).decode()}"},
    )
    print(event_id)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=15) as res:
            print(res.status, res.read().decode())
    except urllib.error.HTTPError as e:
        print(e.code, e.read().decode())


if __name__ == "__main__":
    main()
