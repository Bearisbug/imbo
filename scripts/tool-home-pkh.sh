#!/bin/sh
# Isolated HOME for the pi, Kimi Code and Hermes Agent test sessions
# (docs/TEST.md §3), connected to Imbo like `imbo setup` does.
#   DEEPSEEK_API_KEY=… crates/scripts/tool-home-pkh.sh <dir> <IMBO_HOME>
# The tools are the copies §3 installs under ~/.cache/imbo-test/tools;
# <dir>/bin links them. The key goes only into the tools' own config files
# under <dir> and into <dir>/.deepseek-key, which <dir>/with-key reads.
set -eu
TP=$1 IH=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
I="$ROOT/target/debug/imbo"
TOOLS=$HOME/.cache/imbo-test/tools
: "${DEEPSEEK_API_KEY:?}"
mkdir -p "$TP/bin" "$TP/.pi/agent" "$TP/.kimi-code" "$TP/.hermes"
ln -sfn "$TOOLS/pi/node_modules/.bin/pi" "$TP/bin/pi"
ln -sfn "$TOOLS/kimi/node_modules/.bin/kimi" "$TP/bin/kimi"
ln -sfn "$TOOLS/hermes/venv/bin/hermes" "$TP/bin/hermes"
ln -sfn "$(command -v node)" "$TP/bin/node"
umask 077
printf %s "$DEEPSEEK_API_KEY" > "$TP/.deepseek-key"
cat > "$TP/with-key" <<EOF
#!/bin/sh
DEEPSEEK_API_KEY=\$(cat "$TP/.deepseek-key") exec "\$@"
EOF
chmod 700 "$TP/with-key"

cat > "$TP/.pi/agent/settings.json" <<EOF
{"defaultProvider":"deepseek","defaultModel":"deepseek-flash","enableInstallTelemetry":false,"quietStartup":true}
EOF
cat > "$TP/.kimi-code/config.toml" <<EOF
default_model = "deepseek-flash"
telemetry = false

[providers.deepseek]
type = "openai"
base_url = "https://api.deepseek.com/v1"
api_key = "$DEEPSEEK_API_KEY"

[models.deepseek-flash]
provider = "deepseek"
model = "deepseek-flash"
max_context_size = 128000
EOF
cat > "$TP/.hermes/config.yaml" <<EOF
model:
  provider: deepseek
  default: deepseek-flash
telemetry:
  shared_metrics:
    enabled: false
    send: false
    offer_version: 2
EOF
printf 'DEEPSEEK_API_KEY=%s\n' "$DEEPSEEK_API_KEY" > "$TP/.hermes/.env"

for t in pi kimi-code hermes; do
  env -i HOME="$TP" PATH="$TP/bin:/usr/bin:/bin" IMBO_HOME="$IH" "$I" dev connect $t
done
