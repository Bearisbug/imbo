#!/bin/sh
# Alipay time pack settings for the local server (docs/TEST.md §3 支付宝时长包).
# Source it after local-env.sh and billing-env.sh (time packs count only with
# Waffo), before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/watcha-env.sh
# Watcha can't reach a local server and takes real money, so watcha-fake.py
# answers the access check on 127.0.0.1:18092; the server asks that fake only
# with IMBO_DEV=1, which local-env.sh sets. `local-env.sh down` stops it.
: "${L:?source crates/scripts/local-env.sh first}"
WF="$(cd "$(dirname "${BASH_SOURCE:-$0}")" && pwd)/watcha-fake.py"
mkdir -p "$L/watcha"
if ! { [ -s "$L/watcha/serve.pid" ] && kill -0 "$(cat "$L/watcha/serve.pid")" 2>/dev/null; }; then
  (cd "$L/watcha" && WATCHA_DIR="$L/watcha" nohup "$WF" serve >> "$L/watcha/serve.log" 2>&1 &)
  until curl -s --noproxy '*' -o /dev/null http://127.0.0.1:18092/; do sleep 0.2; done
fi
export WATCHA_DIR="$L/watcha"
export IMBO_WATCHA_API_KEY=sandbox-local-key
export IMBO_WATCHA_ENTITLEMENT_ID=ent_local_timepack
export IMBO_WATCHA_API_URL=http://127.0.0.1:18092
