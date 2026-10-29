#!/bin/sh
# App Store settings for the local server (docs/TEST.md §3 应用内购买). Source
# it after local-env.sh and billing-env.sh (Apple counts only with Waffo),
# before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/apple-env.sh
# The first time it makes $L/apple/ with `apple-event.py root` and starts
# the fake App Store Server API on 127.0.0.1:18091 in the background; the
# server trusts that root and asks that fake only with IMBO_DEV=1, which
# local-env.sh sets. `local-env.sh down` stops the fake and deletes $L.
: "${L:?source crates/scripts/local-env.sh first}"
AE="$(cd "$(dirname "${BASH_SOURCE:-$0}")" && pwd)/apple-event.py"
[ -s "$L/apple/inapp-key.p8" ] || "$AE" root > /dev/null
if ! { [ -s "$L/apple/serve.pid" ] && kill -0 "$(cat "$L/apple/serve.pid")" 2>/dev/null; }; then
  (cd "$L/apple" && nohup "$AE" serve >> "$L/apple/serve.log" 2>&1 &)
  until curl -s --noproxy '*' -o /dev/null http://127.0.0.1:18091/; do sleep 0.2; done
fi
export IMBO_APPLE_BUNDLE_ID=dev.imbo.app
export IMBO_APPLE_APP_ID=1000000001
export IMBO_APPLE_PRODUCT_MONTHLY=dev.imbo.app.monthly
export IMBO_APPLE_PRODUCT_YEARLY=dev.imbo.app.yearly
export IMBO_APPLE_KEY_ID=LOCALKEY01
export IMBO_APPLE_ISSUER_ID=00000000-0000-4000-8000-000000000001
IMBO_APPLE_PRIVATE_KEY=$(cat "$L/apple/inapp-key.p8")
export IMBO_APPLE_PRIVATE_KEY
export IMBO_APPLE_ROOT_CERT="$L/apple/root.pem"
export IMBO_APPLE_API_URL=http://127.0.0.1:18091
