#!/bin/sh
# Sign macOS binaries with the Developer ID certificate, so macOS knows every release as the same program
# and keeps the folder permissions the user already gave it.
#   IMBO_DEVELOPER_ID_KEY=<the certificate's private key, PEM> scripts/sign-macos.sh <binary>…
# scripts/developer-id.pem holds the certificate and Apple's intermediate; both are public.
set -eu
[ $# -gt 0 ] || { echo "usage: $0 <binary>…" >&2; exit 2; }
: "${IMBO_DEVELOPER_ID_KEY:?set it to the private key of scripts/developer-id.pem}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CERTS="$ROOT/scripts/developer-id.pem"
TEAM=4LFCZ9DT3W
# macOS ties the permissions to this name and the team; a release signed under another name is asked again.
IDENTIFIER=dev.imbo.cli

umask 077
work=$(mktemp -d)
keychain="$work/sign.keychain-db"
searched=$(security list-keychains -d user | tr '\n' ' ')
cleanup() {
  eval "security list-keychains -d user -s $searched" || true
  security delete-keychain "$keychain" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# /usr/bin/openssl on purpose: `security import` can't read the p12 that OpenSSL 3 writes by default.
P12_PASS=$(/usr/bin/openssl rand -hex 24)
export P12_PASS
printf '%s\n' "$IMBO_DEVELOPER_ID_KEY" |
  /usr/bin/openssl pkcs12 -export -inkey /dev/stdin -in "$CERTS" -out "$work/id.p12" -passout env:P12_PASS
security create-keychain -p "$P12_PASS" "$keychain"
security set-keychain-settings "$keychain"
security unlock-keychain -p "$P12_PASS" "$keychain"
security import "$work/id.p12" -k "$keychain" -P "$P12_PASS" -T /usr/bin/codesign > /dev/null
security set-key-partition-list -S apple-tool:,apple: -s -k "$P12_PASS" "$keychain" > /dev/null
# codesign only finds an identity in a keychain that is on the search list.
eval "security list-keychains -d user -s \"$keychain\" $searched"

identity=$(/usr/bin/openssl x509 -in "$CERTS" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')
for bin in "$@"; do
  codesign --force --sign "$identity" --identifier "$IDENTIFIER" --options runtime --timestamp "$bin"
  codesign --verify --strict "$bin"
  requirement=$(codesign -d -r- "$bin" 2>&1)
  case "$requirement" in
    *"identifier \"$IDENTIFIER\""*"certificate leaf[subject.OU] = \"$TEAM\""*) echo "signed $bin" ;;
    *) echo "$bin isn't signed as $IDENTIFIER of team $TEAM: $requirement" >&2; exit 1 ;;
  esac
done
