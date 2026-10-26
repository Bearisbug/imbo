#!/bin/sh
# Payment settings for the local server (docs/TEST.md §3). Source it after
# local-env.sh, before `local-env.sh up` or `local-env.sh server`:
#   . crates/scripts/billing-env.sh [billing start, default 2026-09-01]
# The API key is Creem's test key from Bitwarden. The webhook secret is made
# up here: Creem can't reach a local server, so only creem-event.py signs with it.
mkdir -p "$L"
[ -s "$L/webhook-secret" ] || (umask 077 && openssl rand -hex 24 > "$L/webhook-secret")
IMBO_CREEM_API_KEY=$(bw get password CREEM_TEST_API_KEY)
IMBO_CREEM_WEBHOOK_SECRET=$(cat "$L/webhook-secret")
export IMBO_CREEM_API_KEY IMBO_CREEM_WEBHOOK_SECRET
export IMBO_CREEM_PRODUCT_MONTHLY=prod_2AgIAccHzcRdBOQfKLeDeX
export IMBO_CREEM_PRODUCT_YEARLY=prod_6Oo7L19kFJE30iR92KdCdm
export IMBO_BILLING_START="${1:-2026-09-01}"
export IMBO_WEB_ORIGIN=https://app.imbo.dev
