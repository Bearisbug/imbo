#!/bin/sh
# Local test environment (docs/TEST.md §3).
#   . crates/scripts/local-env.sh            # variables only
#   crates/scripts/local-env.sh up [contacts] # server + seed + three daemons, e.g. up alice:bob,alice:carol
#   crates/scripts/local-env.sh down          # stop everything and delete $L
# $L must stay short: macOS limits Unix socket paths to 104 bytes, and
# Codex's control socket lives three levels under $L/codex-home.
ROOT=$(cd "$(dirname "${BASH_SOURCE:-$0}")/.." && pwd)
export L=/tmp/imbo-t TH=/tmp/imbo-t/th
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
  CODEX_HOME=$ch IMBO_HOME=$L/$1 nohup "$I" daemon >> "$L/$1/daemon.log" 2>&1 &
}

case "${1:-}" in
  up)
    mkdir -p "$L/work" "$CODEX_HOME"
    (cd "$L" && IMBO_DATA_DIR=$L/server IMBO_LISTEN=127.0.0.1:18080 IMBO_MAIL_LOG_ONLY=1 IMBO_DEV=1 \
      nohup "$S" >> "$L/server.log" 2>&1 &)
    until curl -sf http://127.0.0.1:18080/v1/health >/dev/null; do sleep 0.3; done
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
  down)
    tmux -L imbo-test kill-server 2>/dev/null
    pkill -f "$I daemon"; pkill -f "$I mcp"; pkill -f "$I hook"; pkill -f "$I wait"; pkill -f "^$S"
    # Tools started with HOME=$TH, such as opencode's background service.
    ps -axE -o pid=,command= | grep "HOME=$TH " | grep -v grep | awk '{print $1}' | xargs kill 2>/dev/null
    ps -ax -o pid=,command= | grep "$L/codex-home" | grep -v grep | awk '{print $1}' | xargs kill 2>/dev/null
    sleep 1
    rm -rf "$L"
    ;;
esac
