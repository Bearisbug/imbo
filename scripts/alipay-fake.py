#!/usr/bin/env python3
"""A fake Alipay gateway for local tests (docs/TEST.md §3 支付宝时长包).

  alipay-fake.py keys                  make the app key pair the server signs
                                       with and this fake's own key pair
  alipay-fake.py serve                 the gateway on 18093: a GET of a signed
                                       alipay.trade.page.pay URL shows a
                                       cashier; POST alipay.trade.query answers
  alipay-fake.py open <url or order string>
                                       check the server's signature and keep
                                       the order, as the cashier or the App's
                                       Alipay SDK would; prints its fields
  alipay-fake.py pay <out_trade_no> [--status S] [--amount A] [--app ID]
                     [--paid-at "yyyy-mm-dd HH:MM:SS"] [--quiet]
                                       the buyer pays; notifies the server
                                       unless --quiet
  alipay-fake.py notify <out_trade_no> [--status S] [--amount A] [--app ID]
                        [--notify-id N] [--forge]
                                       send one notification; the same
                                       notify id again is a resend
  alipay-fake.py fail <500|slow|error|off>
                                       how the next order queries go wrong
  alipay-fake.py notifications <on|off>
                                       whether paying notifies the server

State and keys are in $ALIPAY_DIR. Notifications go to $ALIPAY_NOTIFY_URL,
by default the local server's. The server uses this fake only when
IMBO_ALIPAY_API_URL points at it, which needs IMBO_DEV=1; alipay-env.sh sets
both. Every command prints one JSON line.
"""

import argparse
import base64
import datetime as dt
import hashlib
import html
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 18093
NOTIFY_URL = os.environ.get("ALIPAY_NOTIFY_URL", "http://127.0.0.1:18080/v1/billing/alipay/notify")
BEIJING = dt.timezone(dt.timedelta(hours=8))
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def path(name):
    return os.path.join(os.environ.get("ALIPAY_DIR", "."), name)


def load():
    try:
        with open(path("state.json")) as f:
            return json.load(f)
    except FileNotFoundError:
        return {"orders": {}, "fail": "off", "notifications": "on", "seq": 0}


def save(state):
    with open(path("state.json"), "w") as f:
        json.dump(state, f)


def now_text():
    return dt.datetime.now(BEIJING).strftime("%Y-%m-%d %H:%M:%S")


# ---------- signatures (RSA2 by openssl) ----------

def signing_string(params, skip):
    return "&".join(f"{k}={v}" for k, v in sorted(params.items()) if v != "" and k not in skip)


def rsa_sign(key_file, text):
    p = subprocess.run(["openssl", "dgst", "-sha256", "-sign", key_file],
                       input=text.encode(), capture_output=True, check=True)
    return base64.b64encode(p.stdout).decode()


def rsa_verify(pub_file, text, sign):
    try:
        sig = base64.b64decode(sign, validate=True)
    except ValueError:
        return False
    with tempfile.NamedTemporaryFile() as f:
        f.write(sig)
        f.flush()
        p = subprocess.run(["openssl", "dgst", "-sha256", "-verify", pub_file, "-signature", f.name],
                           input=text.encode(), capture_output=True)
    return p.returncode == 0


def server_signed(params):
    """A request from the server is signed over everything but `sign`."""
    return rsa_verify(path("app.pub.pem"), signing_string(params, {"sign"}), params.get("sign", ""))


# ---------- orders ----------

def open_order(params):
    """Keep the order a signed page.pay URL or app.pay order string makes."""
    ok = server_signed(params)
    biz = json.loads(params.get("biz_content") or "{}")
    fields = {
        "sign_ok": ok,
        "method": params.get("method"),
        "app_id": params.get("app_id"),
        "out_trade_no": biz.get("out_trade_no"),
        "total_amount": biz.get("total_amount"),
        "subject": biz.get("subject"),
        "product_code": biz.get("product_code"),
        "timeout_express": biz.get("timeout_express"),
        "return_url": params.get("return_url"),
        "notify_url": params.get("notify_url"),
    }
    if ok and fields["out_trade_no"]:
        state = load()
        old = state["orders"].get(fields["out_trade_no"], {})
        state["orders"][fields["out_trade_no"]] = {**fields, "status": old.get("status")}
        save(state)
    return fields


def notification(out_trade_no, status="TRADE_SUCCESS", amount=None, app=None, notify_id=None,
                 paid_at=None, forge=False):
    """Send one notification, signed with this fake's key; the server's answer."""
    state = load()
    order = state["orders"].get(out_trade_no, {})
    state["seq"] += 1
    save(state)
    params = {
        "notify_time": now_text(),
        "notify_type": "trade_status_sync",
        "notify_id": notify_id or f"fake{int(time.time() * 1000)}{state['seq']}",
        "charset": "utf-8",
        "version": "1.0",
        "sign_type": "RSA2",
        "app_id": app or order.get("app_id") or "",
        "auth_app_id": app or order.get("app_id") or "",
        "trade_no": "2026" + hashlib.sha256(out_trade_no.encode()).hexdigest()[:20],
        "out_trade_no": out_trade_no,
        "trade_status": status,
        "total_amount": amount or order.get("total_amount") or "15.00",
        "receipt_amount": amount or order.get("total_amount") or "15.00",
        "subject": order.get("subject") or "Imbo",
        "buyer_id": "2088000000000001",
        "seller_id": "2088000000000002",
        "gmt_create": paid_at or now_text(),
        "gmt_payment": paid_at or now_text(),
    }
    params["sign"] = rsa_sign(path("app.pem" if forge else "alipay.pem"),
                              signing_string(params, {"sign", "sign_type"}))
    data = urllib.parse.urlencode(params).encode()
    r = urllib.request.Request(NOTIFY_URL, data=data, method="POST")
    r.add_header("content-type", "application/x-www-form-urlencoded; charset=utf-8")
    try:
        with HTTP.open(r, timeout=30) as res:
            return {"status": res.status, "body": res.read().decode(errors="replace"),
                    "notify_id": params["notify_id"]}
    except urllib.error.HTTPError as e:
        return {"status": e.code, "body": e.read().decode(errors="replace"),
                "notify_id": params["notify_id"]}


def pay(out_trade_no, status="TRADE_SUCCESS", amount=None, app=None, paid_at=None, quiet=False):
    state = load()
    order = state["orders"].setdefault(out_trade_no, {"out_trade_no": out_trade_no})
    order["status"] = status
    order["paid_at"] = paid_at or now_text()
    if amount:
        order["total_amount"] = amount
    if app:
        order["app_id"] = app
    save(state)
    if quiet or state.get("notifications") == "off":
        return {"paid": out_trade_no, "notified": None}
    return {"paid": out_trade_no,
            "notified": notification(out_trade_no, status, amount, app, paid_at=order["paid_at"])}


# ---------- the gateway ----------

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, code, body, ctype="application/json; charset=utf-8", headers=()):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(data)))
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        if url.path != "/gateway.do":
            self.reply(200, "fake alipay", "text/plain")
            return
        params = dict(urllib.parse.parse_qsl(url.query, keep_blank_values=True))
        order = open_order(params)
        if not order["sign_ok"]:
            self.reply(400, "<h1>invalid-signature</h1>", "text/html; charset=utf-8")
            return
        e = {k: html.escape(str(v or "")) for k, v in order.items()}
        page = f"""<!doctype html><meta charset="utf-8"><title>支付宝收银台（本机假服务）</title>
<h1>支付宝收银台（本机假服务）</h1>
<p>订单号 <code>{e['out_trade_no']}</code></p>
<p>{e['subject']}，¥{e['total_amount']}</p>
<form method="post" action="/cashier/pay"><input type="hidden" name="out_trade_no" value="{e['out_trade_no']}">
<button type="submit">确认付款</button></form>"""
        self.reply(200, page, "text/html; charset=utf-8")

    def do_POST(self):
        url = urllib.parse.urlsplit(self.path)
        length = int(self.headers.get("content-length") or 0)
        params = dict(urllib.parse.parse_qsl(self.rfile.read(length).decode(), keep_blank_values=True))
        if url.path == "/cashier/pay":
            self.cashier_pay(params.get("out_trade_no", ""))
        elif url.path == "/gateway.do":
            self.query(params)
        else:
            self.reply(404, "{}")

    def cashier_pay(self, out_trade_no):
        order = load()["orders"].get(out_trade_no)
        if not order:
            self.reply(404, "no such order", "text/plain")
            return
        pay(out_trade_no)
        back = {
            "charset": "utf-8", "method": "alipay.trade.page.pay.return", "out_trade_no": out_trade_no,
            "total_amount": order.get("total_amount") or "", "app_id": order.get("app_id") or "",
            "auth_app_id": order.get("app_id") or "", "trade_no": "2026fake", "seller_id": "2088000000000002",
            "timestamp": now_text(), "version": "1.0", "sign_type": "RSA2",
        }
        back["sign"] = rsa_sign(path("alipay.pem"), signing_string(back, {"sign", "sign_type"}))
        target = order.get("return_url") or "about:blank"
        sep = "&" if "?" in target else "?"
        self.reply(302, "", "text/plain", [("location", target + sep + urllib.parse.urlencode(back))])

    def query(self, params):
        name = "alipay_trade_query_response"
        state = load()
        fail = state.get("fail", "off")
        if fail == "slow":
            time.sleep(8)
        if fail == "500":
            self.reply(500, "busy", "text/plain")
            return
        if not server_signed(params):
            body = {"code": "40002", "msg": "Invalid Arguments", "sub_code": "isv.invalid-signature"}
        elif params.get("method") != "alipay.trade.query":
            body = {"code": "40002", "msg": "Invalid Arguments", "sub_code": "isv.invalid-method"}
        elif fail == "error":
            body = {"code": "20000", "msg": "Service Currently Unavailable", "sub_code": "isp.unknow-error"}
        else:
            out_trade_no = json.loads(params.get("biz_content") or "{}").get("out_trade_no")
            order = state["orders"].get(out_trade_no) or {}
            if not order.get("status"):
                body = {"code": "40004", "msg": "Business Failed", "sub_code": "ACQ.TRADE_NOT_EXIST",
                        "sub_msg": "交易不存在"}
            else:
                body = {"code": "10000", "msg": "Success", "out_trade_no": out_trade_no,
                        "trade_no": "2026fake", "trade_status": order["status"],
                        "total_amount": order.get("total_amount") or "15.00"}
                if order["status"] in ("TRADE_SUCCESS", "TRADE_FINISHED"):
                    body["send_pay_date"] = order.get("paid_at")
        self.reply(200, json.dumps({name: body, "sign": "fake"}, ensure_ascii=False))


# ---------- commands ----------

def keys():
    for name in ("app", "alipay"):
        pem = path(f"{name}.pem")
        if not os.path.exists(pem):
            subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048",
                            "-out", pem], capture_output=True, check=True)
            os.chmod(pem, 0o600)
        subprocess.run(["openssl", "pkey", "-in", pem, "-pubout", "-out", path(f"{name}.pub.pem")],
                       capture_output=True, check=True)
    return {"keys": os.environ.get("ALIPAY_DIR", ".")}


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("keys")
    sub.add_parser("serve")
    o = sub.add_parser("open")
    o.add_argument("signed")
    for name in ("pay", "notify"):
        p = sub.add_parser(name)
        p.add_argument("out_trade_no")
        p.add_argument("--status", default="TRADE_SUCCESS")
        p.add_argument("--amount")
        p.add_argument("--app")
        if name == "pay":
            p.add_argument("--paid-at")
            p.add_argument("--quiet", action="store_true")
        else:
            p.add_argument("--notify-id")
            p.add_argument("--forge", action="store_true")
    f = sub.add_parser("fail")
    f.add_argument("how", choices=["500", "slow", "error", "off"])
    n = sub.add_parser("notifications")
    n.add_argument("on", choices=["on", "off"])
    a = ap.parse_args()

    if a.cmd == "serve":
        with open(path("serve.pid"), "w") as fpid:
            fpid.write(str(os.getpid()))
        ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
        return
    if a.cmd == "keys":
        out = keys()
    elif a.cmd == "open":
        query = urllib.parse.urlsplit(a.signed).query if "?" in a.signed else a.signed
        out = open_order(dict(urllib.parse.parse_qsl(query, keep_blank_values=True)))
    elif a.cmd == "pay":
        out = pay(a.out_trade_no, a.status, a.amount, a.app, a.paid_at, a.quiet)
    elif a.cmd == "notify":
        out = notification(a.out_trade_no, a.status, a.amount, a.app, a.notify_id, forge=a.forge)
    elif a.cmd == "fail":
        state = load()
        state["fail"] = a.how
        save(state)
        out = {"fail": a.how}
    else:
        state = load()
        state["notifications"] = a.on
        save(state)
        out = {"notifications": a.on}
    print(json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    sys.exit(main())
