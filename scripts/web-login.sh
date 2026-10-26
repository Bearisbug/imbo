#!/bin/sh
# Sign in to the local server the way the web app does (docs/TEST.md §3):
#   crates/scripts/web-login.sh <email> <cookie jar> [name]
# Asks for a code, reads it from $L/server.log, posts it with the web app's
# Origin and keeps the imbo_session cookie in the jar. Prints the body, then
# the HTTP status.
: "${L:?source crates/scripts/local-env.sh first}"
U=http://127.0.0.1:18080
before=$(grep -c "mail-log-only code=.* email=$1\$" "$L/server.log")
curl -s --noproxy '*' -X POST $U/v1/auth/code -H 'content-type: application/json' \
  -d "{\"email\":\"$1\"}" > /dev/null
n=0
until [ "$(grep -c "mail-log-only code=.* email=$1\$" "$L/server.log")" -gt "$before" ]; do
  n=$((n + 1)); [ $n -gt 25 ] && { echo "no code for $1 (asked again within a minute?)" >&2; exit 1; }
  sleep 0.2
done
code=$(grep "mail-log-only code=.* email=$1\$" "$L/server.log" | tail -1 | sed 's/.*code=\([0-9]*\).*/\1/')
body="{\"email\":\"$1\",\"code\":\"$code\"${3:+,\"name\":\"$3\"}}"
curl -s --noproxy '*' -X POST $U/v1/web/session -H 'content-type: application/json' \
  -H 'Origin: https://app.imbo.dev' -d "$body" -c "$2" -w '\n%{http_code}\n'
