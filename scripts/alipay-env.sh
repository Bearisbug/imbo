#!/bin/sh
# Alipay time pack settings for the local server (docs/TEST.md §3 支付宝时长包).
# Source it after local-env.sh and billing-env.sh (time packs count only with
# Waffo), before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/alipay-env.sh
# Alipay can't reach a local server, so alipay-fake.py stands in for its
# gateway and cashier on 127.0.0.1:18093 and signs its notifications with a
# key made here; the server asks that fake only with IMBO_DEV=1, which
# local-env.sh sets. `local-env.sh down` stops it.
: "${L:?source crates/scripts/local-env.sh first}"
AF="$(cd "$(dirname "${BASH_SOURCE:-$0}")" && pwd)/alipay-fake.py"
export ALIPAY_DIR="$L/alipay"
mkdir -p "$ALIPAY_DIR"
[ -s "$ALIPAY_DIR/alipay.pub.pem" ] || "$AF" keys > /dev/null
if ! { [ -s "$ALIPAY_DIR/serve.pid" ] && kill -0 "$(cat "$ALIPAY_DIR/serve.pid")" 2>/dev/null; }; then
  (cd "$ALIPAY_DIR" && nohup "$AF" serve >> "$ALIPAY_DIR/serve.log" 2>&1 &)
  until curl -s --noproxy '*' -o /dev/null http://127.0.0.1:18093/; do sleep 0.2; done
fi
export IMBO_ALIPAY_APP_ID=9021000000000001
export IMBO_ALIPAY_MOBILE_APP_ID=9021000000000002
IMBO_ALIPAY_PRIVATE_KEY=$(cat "$ALIPAY_DIR/app.pem")
IMBO_ALIPAY_PUBLIC_KEY=$(cat "$ALIPAY_DIR/alipay.pub.pem")
export IMBO_ALIPAY_PRIVATE_KEY IMBO_ALIPAY_PUBLIC_KEY
export IMBO_ALIPAY_API_URL=http://127.0.0.1:18093/gateway.do
