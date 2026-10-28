#!/bin/sh
# Payment settings for the local server (docs/TEST.md §3). Source it after
# local-env.sh, before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/billing-env.sh [billing start, default 2026-09-01]
# Waffo's test environment; the API key is the test private key from
# Bitwarden. Waffo can't reach a local server, so waffo-event.py signs
# webhooks with a key made here and the server checks its public half. For
# Waffo's own webhooks through a tunnel, `unset IMBO_WAFFO_WEBHOOK_PUBLIC_KEY`.
mkdir -p "$L"
[ -s "$L/webhook-key.pem" ] || (umask 077 &&
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$L/webhook-key.pem" 2>/dev/null)
IMBO_WAFFO_PRIVATE_KEY=$(bw get password IMBO_WAFFO_TEST_PRIVATE_KEY)
IMBO_WAFFO_WEBHOOK_PUBLIC_KEY=$(openssl pkey -in "$L/webhook-key.pem" -pubout)
export IMBO_WAFFO_PRIVATE_KEY IMBO_WAFFO_WEBHOOK_PUBLIC_KEY
export IMBO_WAFFO_ENV=test
export IMBO_WAFFO_MERCHANT_ID=MER_6AB5i2EKvRW2bYXYpYnFPR
export IMBO_WAFFO_STORE_ID=STO_2Wn6wV82T3eH4k9C4fyuvz
export IMBO_WAFFO_PRODUCT_MONTHLY=PROD_3eEX6t7or9rfen01IivKto
export IMBO_WAFFO_PRODUCT_YEARLY=PROD_7g8P6Z0bIdkpCwNw8eDvzm
export IMBO_BILLING_START="${1:-2026-09-01}"
export IMBO_WEB_ORIGIN=https://app.imbo.dev
