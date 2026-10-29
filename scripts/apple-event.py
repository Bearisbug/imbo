#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["cryptography>=44"]
# ///
"""A local stand-in for the App Store (docs/TEST.md §3 应用内购买).

  apple-event.py root
  apple-event.py serve
  apple-event.py token <account ID>
  apple-event.py transaction --account <ID> --original <N> [--env Xcode|Sandbox|Production] [--submit <name>] ...
  apple-event.py notify <TYPE> [--subtype <SUBTYPE>] --original <N> [--account <ID>] ...
  apple-event.py status <N> [--state active|grace|retry|expired|revoked] [--auto-renew on|off] ...

`root` makes $L/apple/: a root, an intermediate and a signing certificate
shaped like Apple's (the intermediate and the signer carry Apple's two marker
extensions), and an In-App Purchase key. The server trusts the root through
IMBO_APPLE_ROOT_CERT and signs its API requests with the key
(apple-env.sh exports both). `serve` answers the App Store Server API's Get
All Subscription Statuses on 127.0.0.1:18091 under /sandbox and /production
with what `status` set, and logs `<env> <original> <code>` per request to
$L/apple/fake.log.
"""
import argparse
import base64
import datetime as dt
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import (
    decode_dss_signature,
    encode_dss_signature,
)
from cryptography.x509.oid import NameOID

L = os.environ.get("L", "/tmp/imbo-t")
D = os.path.join(L, "apple")
BUNDLE = "dev.imbo.app"
APP_APPLE_ID = 1000000001
PRODUCTS = {"monthly": "dev.imbo.app.monthly", "yearly": "dev.imbo.app.yearly"}
GROUP = "21000001"
SERVER = "http://127.0.0.1:18080"
PORT = 18091
STATUS = {"active": 1, "expired": 2, "retry": 3, "grace": 4, "revoked": 5}
WWDR_OID = x509.ObjectIdentifier("1.2.840.113635.100.6.2.1")
SIGNER_OID = x509.ObjectIdentifier("1.2.840.113635.100.6.11.1")


def path(name):
    return os.path.join(D, name)


def b64url(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def unb64url(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def ms(t):
    return int(t.timestamp() * 1000)


def when(text, now):
    """ISO time, or relative to now: +30d, -1d, -2h, -1m, +0m."""
    m = re.fullmatch(r"([+-]\d+)([dhms])", text)
    if m:
        unit = {"d": "days", "h": "hours", "m": "minutes", "s": "seconds"}[m.group(2)]
        return now + dt.timedelta(**{unit: int(m.group(1))})
    return dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def account_token(account):
    return str(uuid.uuid5(uuid.uuid5(uuid.NAMESPACE_DNS, "imbo.dev"), str(account)))


def product_id(name):
    return PRODUCTS.get(name, name)


# ---- certificates ----


def cert(subject, key, issuer, issuer_key, ca, marker, days, hash_alg):
    now = dt.datetime.now(dt.timezone.utc)
    b = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, subject),
                                 x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Imbo local test")]))
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - dt.timedelta(days=1))
        .not_valid_after(now + dt.timedelta(days=days))
        .add_extension(x509.BasicConstraints(ca=ca, path_length=None), critical=True)
        .add_extension(x509.KeyUsage(
            digital_signature=not ca, content_commitment=False, key_encipherment=False,
            data_encipherment=False, key_agreement=False, key_cert_sign=ca, crl_sign=ca,
            encipher_only=False, decipher_only=False), critical=True)
    )
    if marker:
        b = b.add_extension(x509.UnrecognizedExtension(marker, b"\x05\x00"), critical=False)
    return b.sign(issuer_key, hash_alg)


def write_key(name, key):
    with open(path(name), "wb") as f:
        f.write(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                  serialization.NoEncryption()))


def write_cert(name, c):
    with open(path(name), "wb") as f:
        f.write(c.public_bytes(serialization.Encoding.PEM))


def cmd_root(_args):
    os.makedirs(D, exist_ok=True)
    # Apple: Root CA - G3 (P-384) → WWDR intermediate (P-384) → signer (P-256), SHA-384.
    root_key = ec.generate_private_key(ec.SECP384R1())
    root_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Imbo Test Root CA"),
                           x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Imbo local test")])
    root = cert("Imbo Test Root CA", root_key, root_name, root_key, True, None, 3650, hashes.SHA384())
    wwdr_key = ec.generate_private_key(ec.SECP384R1())
    wwdr = cert("Imbo Test WWDR", wwdr_key, root.subject, root_key, True, WWDR_OID, 3650, hashes.SHA384())
    signer_key = ec.generate_private_key(ec.SECP256R1())
    signer = cert("Imbo Test StoreKit Signer", signer_key, wwdr.subject, wwdr_key, False, SIGNER_OID,
                  730, hashes.SHA384())
    # Xcode's StoreKit local testing signs with its own self-signed certificate.
    xcode_key = ec.generate_private_key(ec.SECP256R1())
    xcode_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "StoreKit Testing in Xcode")])
    xcode = cert("StoreKit Testing in Xcode", xcode_key, xcode_name, xcode_key, True, None, 730,
                 hashes.SHA256())
    for name, key in [("root-key.pem", root_key), ("wwdr-key.pem", wwdr_key),
                      ("signer-key.pem", signer_key), ("xcode-key.pem", xcode_key),
                      ("inapp-key.p8", ec.generate_private_key(ec.SECP256R1()))]:
        write_key(name, key)
    for name, c in [("root.pem", root), ("wwdr.pem", wwdr), ("signer.pem", signer), ("xcode.pem", xcode)]:
        write_cert(name, c)
    print(D)


def load_key(name):
    with open(path(name), "rb") as f:
        return serialization.load_pem_private_key(f.read(), None)


def load_cert(name):
    with open(path(name), "rb") as f:
        return x509.load_pem_x509_certificate(f.read())


# ---- JWS ----


def jws(payload, xcode=False, bad=False):
    """ES256 compact JWS with the certificate chain in x5c, as Apple signs."""
    names = ["xcode.pem"] if xcode else ["signer.pem", "wwdr.pem", "root.pem"]
    x5c = [base64.b64encode(load_cert(n).public_bytes(serialization.Encoding.DER)).decode() for n in names]
    header = b64url(json.dumps({"alg": "ES256", "x5c": x5c}).encode())
    body = b64url(json.dumps(payload).encode())
    key = load_key("xcode-key.pem" if xcode else "signer-key.pem")
    r, s = decode_dss_signature(key.sign(f"{header}.{body}".encode(), ec.ECDSA(hashes.SHA256())))
    sig = b64url(r.to_bytes(32, "big") + s.to_bytes(32, "big"))
    if bad:
        # Signed, then changed: the signature no longer covers the content.
        payload = dict(payload, signedDate=payload.get("signedDate", 0) + 1)
        body = b64url(json.dumps(payload).encode())
    return f"{header}.{body}.{sig}"


def transaction(original, tid, product, env, expires, token, bundle, signed, revoked=None):
    now = dt.datetime.now(dt.timezone.utc)
    t = {
        "transactionId": tid,
        "originalTransactionId": original,
        "webOrderLineItemId": tid,
        "bundleId": bundle,
        "productId": product,
        "subscriptionGroupIdentifier": GROUP,
        "purchaseDate": ms(now),
        "originalPurchaseDate": ms(now),
        "expiresDate": ms(expires),
        "quantity": 1,
        "type": "Auto-Renewable Subscription",
        "inAppOwnershipType": "PURCHASED",
        "signedDate": ms(signed),
        "environment": env,
        "transactionReason": "PURCHASE",
        "storefront": "USA",
        "storefrontId": "143441",
        "price": 29990 if product.endswith("yearly") else 2990,
        "currency": "USD",
    }
    if token:
        t["appAccountToken"] = token
    if revoked:
        t["revocationDate"] = ms(revoked)
        t["revocationReason"] = 0
    return t


def renewal(original, product, env, auto_renew, state, expires):
    now = dt.datetime.now(dt.timezone.utc)
    r = {
        "originalTransactionId": original,
        "autoRenewProductId": product,
        "productId": product,
        "autoRenewStatus": 1 if auto_renew else 0,
        "isInBillingRetryPeriod": state in ("retry", "grace"),
        "signedDate": ms(now),
        "environment": env,
        "recentSubscriptionStartDate": ms(now),
        "renewalDate": ms(expires),
    }
    if state == "grace":
        r["gracePeriodExpiresDate"] = ms(expires + dt.timedelta(days=16))
    if not auto_renew:
        r["expirationIntent"] = 1
    return r


def token_of(args):
    if args.no_token:
        return None
    if args.token_of is not None:
        return account_token(args.token_of)
    if args.account is not None:
        return account_token(args.account)
    return None


def post(url, headers, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                 headers={"content-type": "application/json", **headers})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=30) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def cmd_token(args):
    print(account_token(args.account))


def cmd_transaction(args):
    now = dt.datetime.now(dt.timezone.utc)
    t = transaction(args.original, args.id or args.original, product_id(args.product), args.env,
                    when(args.expires, now), token_of(args), args.bundle, now)
    signed = jws(t, xcode=args.env == "Xcode", bad=args.bad_signature)
    if not args.submit:
        print(signed)
        return
    with open(os.path.join(L, f"{args.submit}.app")) as f:
        app = f.read().strip()
    code, body = post(f"{SERVER}/v1/billing/apple/transactions", {"authorization": f"Bearer {app}"},
                      {"signed_transaction": signed})
    print(code)
    print(body)


def cmd_notify(args):
    now = dt.datetime.now(dt.timezone.utc)
    product = product_id(args.product)
    expires = when(args.expires, now)
    t = transaction(args.original, args.original, product, args.env, expires, token_of(args),
                    args.bundle, now)
    n = {
        "notificationType": args.type,
        "notificationUUID": args.uuid or str(uuid.uuid4()),
        "version": "2.0",
        "signedDate": ms(when(args.signed, now)),
        "data": {
            "appAppleId": APP_APPLE_ID,
            "bundleId": args.bundle,
            "bundleVersion": "1",
            "environment": args.env,
            "signedTransactionInfo": jws(t),
            "signedRenewalInfo": jws(renewal(args.original, product, args.env, True, "active", expires)),
            "status": 1,
        },
    }
    if args.subtype:
        n["subtype"] = args.subtype
    code, body = post(f"{args.url.rstrip('/')}/v1/billing/apple/notifications", {},
                      {"signedPayload": jws(n, bad=args.bad_signature)})
    print(code)
    if body:
        print(body)


# ---- the fake App Store Server API ----


def load_state():
    try:
        with open(path("state.json")) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def cmd_status(args):
    now = dt.datetime.now(dt.timezone.utc)
    state = load_state()
    s = state.setdefault(args.original, {"state": "active", "auto_renew": True, "env": "Sandbox",
                                         "product": PRODUCTS["monthly"], "account": None,
                                         "expires": ms(now + dt.timedelta(days=30)), "fail": None})
    if args.state:
        s["state"] = args.state
        if args.state == "revoked":
            s.setdefault("revoked", ms(now))
    if args.auto_renew:
        s["auto_renew"] = args.auto_renew == "on"
    if args.expires:
        s["expires"] = ms(when(args.expires, now))
    if args.account is not None:
        s["account"] = args.account
    if args.product:
        s["product"] = product_id(args.product)
    if args.env:
        s["env"] = args.env
    if args.fail:
        s["fail"] = None if args.fail == "off" else args.fail
    tmp = path("state.json.tmp")
    with open(tmp, "w") as f:
        json.dump(state, f, indent=1)
    os.replace(tmp, path("state.json"))
    print(json.dumps({args.original: s}))


def jwt_ok(header_value):
    """The bearer token is ES256 under the In-App key, for our bundle."""
    if not header_value or not header_value.startswith("Bearer "):
        return False
    parts = header_value[7:].split(".")
    if len(parts) != 3:
        return False
    try:
        raw = unb64url(parts[2])
        sig = encode_dss_signature(int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big"))
        load_key("inapp-key.p8").public_key().verify(sig, f"{parts[0]}.{parts[1]}".encode(),
                                                     ec.ECDSA(hashes.SHA256()))
        claims = json.loads(unb64url(parts[1]))
        header = json.loads(unb64url(parts[0]))
    except (InvalidSignature, ValueError):
        return False
    return (header.get("alg") == "ES256" and claims.get("aud") == "appstoreconnect-v1"
            and claims.get("bid") == BUNDLE and claims.get("exp", 0) > time.time())


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def answer(self, env, original, code, body=None):
        with open(path("fake.log"), "a") as f:
            f.write(f"{env} {original} {code}\n")
        data = json.dumps(body or {}).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        m = re.fullmatch(r"/(sandbox|production)/inApps/v1/subscriptions/([0-9]+)", self.path)
        if not m:
            self.answer("-", self.path, 404, {"errorCode": 4040000, "errorMessage": "Not found"})
            return
        env, original = m.groups()
        if not jwt_ok(self.headers.get("authorization")):
            self.answer(env, original, 401)
            return
        s = load_state().get(original)
        if not s or s["env"].lower() != env:
            self.answer(env, original, 404, {"errorCode": 4040010, "errorMessage": "Transaction id not found."})
            return
        if s.get("fail") == "500":
            self.answer(env, original, 500, {"errorCode": 5000000, "errorMessage": "An unknown error occurred."})
            return
        if s.get("fail") == "slow":
            time.sleep(15)
        if s.get("fail") == "late":
            time.sleep(5)
        now = dt.datetime.now(dt.timezone.utc)
        expires = dt.datetime.fromtimestamp(s["expires"] / 1000, dt.timezone.utc)
        revoked = dt.datetime.fromtimestamp(s["revoked"] / 1000, dt.timezone.utc) \
            if s["state"] == "revoked" else None
        token = account_token(s["account"]) if s.get("account") is not None else None
        t = transaction(original, original, s["product"], s["env"], expires, token, BUNDLE, now, revoked)
        r = renewal(original, s["product"], s["env"], s["auto_renew"], s["state"], expires)
        self.answer(env, original, 200, {
            "environment": s["env"],
            "bundleId": BUNDLE,
            "appAppleId": APP_APPLE_ID,
            "data": [{
                "subscriptionGroupIdentifier": GROUP,
                "lastTransactions": [{
                    "status": STATUS[s["state"]],
                    "originalTransactionId": original,
                    "signedTransactionInfo": jws(t),
                    "signedRenewalInfo": jws(r),
                }],
            }],
        })


def cmd_serve(_args):
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    with open(path("serve.pid"), "w") as f:
        f.write(f"{os.getpid()}\n")
    server.serve_forever()


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("root")
    sub.add_parser("serve")
    t = sub.add_parser("token")
    t.add_argument("account", type=int)

    def common(q, env_choices):
        q.add_argument("--original", required=True)
        q.add_argument("--account", type=int)
        q.add_argument("--token-of", type=int)
        q.add_argument("--no-token", action="store_true")
        q.add_argument("--product", default="monthly")
        q.add_argument("--env", default="Sandbox", choices=env_choices)
        q.add_argument("--expires", default="+30d")
        q.add_argument("--bundle", default=BUNDLE)
        q.add_argument("--bad-signature", action="store_true")

    tx = sub.add_parser("transaction")
    common(tx, ["Xcode", "Sandbox", "Production"])
    tx.add_argument("--id")
    tx.add_argument("--submit")

    n = sub.add_parser("notify")
    n.add_argument("type")
    common(n, ["Sandbox", "Production"])
    n.add_argument("--subtype")
    n.add_argument("--signed", default="+0m")
    n.add_argument("--uuid")
    n.add_argument("--url", default=SERVER)

    s = sub.add_parser("status")
    s.add_argument("original")
    s.add_argument("--state", choices=list(STATUS))
    s.add_argument("--auto-renew", choices=["on", "off"])
    s.add_argument("--expires")
    s.add_argument("--account", type=int)
    s.add_argument("--product")
    s.add_argument("--env", choices=["Sandbox", "Production"])
    s.add_argument("--fail", choices=["500", "slow", "late", "off"])

    args = p.parse_args()
    {"root": cmd_root, "serve": cmd_serve, "token": cmd_token, "transaction": cmd_transaction,
     "notify": cmd_notify, "status": cmd_status}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
