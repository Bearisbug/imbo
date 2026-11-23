#!/bin/sh
# Isolated HOME for the Antigravity CLI test sessions (docs/TEST.md §3),
# connected to Imbo like `imbo setup` does.
#   crates/scripts/tool-home-agy.sh <dir> <IMBO_HOME>
# agy signs in through the real login keychain: its search list for <dir>
# points there. <dir>/work is the sessions' workspace, trusted from the
# start so agy doesn't ask.
set -eu
TA=$1 IH=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
I="$ROOT/target/debug/imbo"
mkdir -p "$TA/bin" "$TA/work" "$TA/Library/Preferences" "$TA/.gemini/antigravity-cli"
ln -sfn "$(command -v agy)" "$TA/bin/agy"
# agy knows the folder by its real path: /tmp is a link on macOS.
printf '{"trustedWorkspaces":["%s"]}\n' "$(cd "$TA/work" && pwd -P)" > "$TA/.gemini/antigravity-cli/settings.json"

KC="$HOME/Library/Keychains/login.keychain-db"
env -i HOME="$TA" /usr/bin/security list-keychains -d user -s "$KC"
env -i HOME="$TA" /usr/bin/security default-keychain -d user -s "$KC"

env -i HOME="$TA" PATH="$TA/bin:/usr/bin:/bin" IMBO_HOME="$IH" "$I" dev connect antigravity
