#!/bin/sh
# Isolated HOME for the DeepSeek Harness test sessions (docs/TEST.md §3),
# connected to Imbo like `imbo setup` does.
#   DEEPSEEK_API_KEY=… crates/scripts/tool-home-dsh.sh <dir> <IMBO_HOME>
# dsh is the copy §3 installs under ~/.cache/imbo-test/tools/dsh; <dir>/bin
# links it. The key goes only into <dir>/.dsh/.env, where dsh reads it.
set -eu
TD=$1 IH=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
I="$ROOT/target/debug/imbo"
TOOLS=$HOME/.cache/imbo-test/tools
: "${DEEPSEEK_API_KEY:?}"
mkdir -p "$TD/bin" "$TD/.dsh"
ln -sfn "$TOOLS/dsh/node_modules/.bin/dsh" "$TD/bin/dsh"
ln -sfn "$(command -v node)" "$TD/bin/node"
umask 077
printf 'DEEPSEEK_API_KEY=%s\n' "$DEEPSEEK_API_KEY" > "$TD/.dsh/.env"

# Session logs go to DeepSeek unless this is off (ADR-048), and the
# default workspace goes under the real ~/Documents unless moved.
cat > "$TD/.dsh/cordis.patch.yml" <<EOF
- id: session-log-deepseek
  name: '@deepseek-ai/dsh-session-log-deepseek'
  config:
    enabled: false
- id: workspace-controller
  name: '@deepseek-ai/dsh-api-workspace-controller'
  config:
    documentsDirectory: $TD/Documents
EOF

env -i HOME="$TD" PATH="$TD/bin:/usr/bin:/bin" IMBO_HOME="$IH" "$I" dev connect deepseek-harness
