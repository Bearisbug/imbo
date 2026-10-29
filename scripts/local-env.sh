#!/bin/sh
# Local test environment (docs/TEST.md §3).
#   . crates/scripts/local-env.sh            # variables only
#   crates/scripts/local-env.sh up [contacts] # server + seed + three daemons, e.g. up alice:bob,alice:carol
#   crates/scripts/local-env.sh server        # restart only the server, with this shell's IMBO_* variables
#   crates/scripts/local-env.sh down          # stop everything and delete $L
# $L must stay short: macOS limits Unix socket paths to 104 bytes, and
# Codex's control socket lives three levels under $L/codex-home.
ROOT=$(cd "$(dirname "${BASH_SOURCE:-$0}")/.." && pwd)
export L=/tmp/imbo-t TH=/tmp/imbo-t/th TP=/tmp/imbo-t/tp
export I="$ROOT/target/debug/imbo" S="$ROOT/target/debug/imbo-server" IMBO="$ROOT/target/debug/imbo"
export M="$ROOT/scripts/mcp-call.py"
unset CLAUDE_CODE_SESSION_ID CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN CLAUDECODE \
  CLAUDE_CODE_ENTRYPOINT CLAUDE_PID CLAUDE_CODE_CHILD_SESSION CODEX_THREAD_ID
export CODEX_HOME=$L/codex-home

# Only bob's daemon sees the test Codex home; the others get an empty one.
start_daemon() {
  ch=$L/$1/codex-none
  [ "$1" = bob ] && ch=$CODEX_HOME
  mkdir -p "$ch"
  # The payment keys are for the server only.
  env -u IMBO_WAFFO_PRIVATE_KEY CODEX_HOME=$ch IMBO_HOME=$L/$1 \
    nohup "$I" daemon >> "$L/$1/daemon.log" 2>&1 &
}

start_server() {
  (cd "$L" || exit
    IMBO_DATA_DIR=$L/server IMBO_LISTEN=127.0.0.1:18080 IMBO_MAIL_LOG_ONLY=1 IMBO_DEV=1 \
      nohup "$S" >> "$L/server.log" 2>&1 &
    echo $! > "$L/server.pid")
  until curl -sf --noproxy '*' http://127.0.0.1:18080/v1/health >/dev/null; do sleep 0.3; done
}

case "${1:-}" in
  up)
    mkdir -p "$L/work" "$CODEX_HOME"
    start_server
    if [ -n "${2:-}" ]; then
      IMBO_DATA_DIR=$L/server "$S" dev seed --homes "$L" --contacts "$2" > "$L/ids.tsv"
    else
      IMBO_DATA_DIR=$L/server "$S" dev seed --homes "$L" > "$L/ids.tsv"
    fi
    for n in alice bob carol; do start_daemon $n; done
    for n in alice bob carol; do
      until grep -q connected "$L/$n/daemon.log" 2>/dev/null; do sleep 0.2; done
    done
    cat "$L/ids.tsv"
    ;;
  daemon)
    start_daemon "$2"
    ;;
  server)
    pid=$(cat "$L/server.pid")
    kill "$pid"
    while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
    start_server
    ;;
  down)
    tmux -L imbo-test kill-server 2>/dev/null
    # The fake App Store of apple-env.sh.
    [ -s "$L/apple/serve.pid" ] && kill "$(cat "$L/apple/serve.pid")" 2>/dev/null
    pkill -f "$I daemon"; pkill -f "$I mcp"; pkill -f "$I hook"; pkill -f "$I wait"; pkill -f "^$S"
    # Tools started with HOME=$TH or $TP, such as opencode's background
    # service and Hermes Agent's gateway.
    ps -axE -o pid=,command= | grep -E "HOME=($TH|$TP) " | grep -v grep | awk '{print $1}' | xargs kill 2>/dev/null
    ps -ax -o pid=,command= | grep "$L/codex-home" | grep -v grep | awk '{print $1}' | xargs kill 2>/dev/null
    sleep 1
    # MiniMax Code makes its plugin hook cache read-only.
    chmod -R u+w "$L" 2>/dev/null
    rm -rf "$L"
    ;;
esac
