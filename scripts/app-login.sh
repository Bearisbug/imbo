#!/bin/sh
# Sign in to the local server the way the App does (docs/TEST.md §3):
#   crates/scripts/app-login.sh <email> [name] [platform]
# Asks for a code, reads it from $L/server.log and posts it to API-048
# without an Origin. Prints the body (with the App token), then the HTTP
# status. platform defaults to ios.
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
body="{\"email\":\"$1\",\"code\":\"$code\",\"platform\":\"${3:-ios}\"${2:+,\"name\":\"$2\"}}"
curl -s --noproxy '*' -X POST $U/v1/app/session -H 'content-type: application/json' \
  -d "$body" -w '\n%{http_code}\n'
