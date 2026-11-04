#!/bin/sh
# Alipay sandbox settings for the local server (docs/TEST.md §3 支付宝沙箱).
# Source it after local-env.sh and billing-env.sh, instead of alipay-env.sh,
# before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/alipay-sandbox-env.sh
# The web app and the App share the sandbox app 9021000170679614; Waffo's
# test mode sends the server to Alipay's sandbox gateway. The sandbox can't
# reach a local server, so its notifications never arrive here: a payment is
# recorded when the order number is checked (API-084).
: "${L:?source crates/scripts/local-env.sh first}"
IMBO_ALIPAY_PRIVATE_KEY=$(bw get password IMBO_ALIPAY_SANDBOX_PRIVATE_KEY)
IMBO_ALIPAY_PUBLIC_KEY=$(bw get password IMBO_ALIPAY_SANDBOX_PUBLIC_KEY)
export IMBO_ALIPAY_PRIVATE_KEY IMBO_ALIPAY_PUBLIC_KEY
export IMBO_ALIPAY_APP_ID=9021000170679614
export IMBO_ALIPAY_MOBILE_APP_ID=9021000170679614
unset IMBO_ALIPAY_API_URL
