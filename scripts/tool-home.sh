#!/bin/sh
# Isolated HOME for the opencode, Cursor, MiniMax Code and ZCode test
# sessions (docs/TEST.md §3), connected to Imbo like `imbo setup` does.
#   DEEPSEEK_API_KEY=… ZCODE_APP=<ZCode.app> crates/scripts/tool-home.sh <dir> <IMBO_HOME>
# The key goes only into the tools' own config files under <dir> and into
# <dir>/.deepseek-key, which <dir>/with-key reads to start opencode.
# Cursor uses the real login keychain: its search list for <dir> points there.
set -eu
TH=$1 IH=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
I="$ROOT/target/debug/imbo"
: "${DEEPSEEK_API_KEY:?}" "${ZCODE_APP:?}"
mkdir -p "$TH/.config/opencode" "$TH/.zcode/v2" "$TH/Applications" "$TH/Library/Preferences"
umask 077
printf %s "$DEEPSEEK_API_KEY" > "$TH/.deepseek-key"
cat > "$TH/with-key" <<EOF
#!/bin/sh
DEEPSEEK_API_KEY=\$(cat "$TH/.deepseek-key") exec "\$@"
EOF
chmod 700 "$TH/with-key"

echo '{"model":"deepseek/deepseek-flash"}' > "$TH/.config/opencode/opencode.json"

HOME=$TH mcode provider add --name deepseek --base-url https://api.deepseek.com \
  --api-format openai-completions --model deepseek-flash --api-key-env DEEPSEEK_API_KEY --use >/dev/null
python3 - "$TH/.minimax/config.yaml" <<'EOF'
import re, sys
p = sys.argv[1]
s = open(p).read()
s = re.sub(r"(dataContribution:\s*\n\s*enabled:\s*)true", r"\1false", s)
open(p, "w").write(s)
EOF

python3 - "$TH/.zcode/v2/provider_config.json" <<'EOF'
import json, os, sys
rule = {"providerId": "ds", "providerName": "DeepSeek", "config": {
    "group": "standard-personal",
    "access": {"type": "api-key", "apiKey": os.environ["DEEPSEEK_API_KEY"]},
    "api": {"type": "openai-chat-completions", "baseUrl": "https://api.deepseek.com"},
    "personalModelIds": ["deepseek-flash"]}}
cfg = {"schemaVersion": 1, "config": {
    "providerConfigRules": {"providerRules": [rule]},
    "modelConfigRules": {"providerModelRules": [], "manualProviderModelRules": []},
    "defaultModelSelection": {"providerId": "ds", "modelId": "deepseek-flash"}}}
json.dump(cfg, open(sys.argv[1], "w"))
EOF
ln -sfn "$ZCODE_APP" "$TH/Applications/ZCode.app"
[ -e "$ZCODE_APP/Contents/Resources/glm/provider" ] || ln -s ../config/provider "$ZCODE_APP/Contents/Resources/glm/provider"

KC="$HOME/Library/Keychains/login.keychain-db"
env -i HOME="$TH" /usr/bin/security list-keychains -d user -s "$KC"
env -i HOME="$TH" /usr/bin/security default-keychain -d user -s "$KC"
if HOME=$TH cursor-agent status 2>&1 | grep -q "Logged in"; then echo "Cursor: logged in"; else echo "Cursor: not logged in" >&2; fi

for t in opencode cursor minimax-code zcode; do
  HOME=$TH IMBO_HOME=$IH "$I" dev connect $t
done
