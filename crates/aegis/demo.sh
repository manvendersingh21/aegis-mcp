#!/bin/sh
set -eu

# Flag --offline sets AEGIS_OFFLINE=1
for arg in "$@"; do
  case "$arg" in
    --offline)
      export AEGIS_OFFLINE=1
      ;;
  esac
done

# cd to repo root
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# Clean up server on exit
SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    echo ""
    echo "Stopping AegisMCP server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

echo "Building target/release/aegis..."
cargo build --release -p aegis

echo "Starting target/release/aegis in background (logs to /tmp/aegis.log)..."
: > /tmp/aegis.log
target/release/aegis > /tmp/aegis.log 2>&1 &
SERVER_PID=$!

PORT="${AEGIS_PORT:-8787}"
echo "Waiting for http://127.0.0.1:${PORT}/healthz..."
READY=0
i=0
while [ "$i" -lt 150 ]; do
  if curl -s -f "http://127.0.0.1:${PORT}/healthz" > /dev/null 2>&1; then
    READY=1
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: Aegis server exited prematurely. Logs:" >&2
    cat /tmp/aegis.log >&2
    exit 1
  fi
  sleep 0.1
  i=$((i + 1))
done

if [ "$READY" -ne 1 ]; then
  echo "ERROR: Timed out waiting for server /healthz. Logs:" >&2
  cat /tmp/aegis.log >&2
  exit 1
fi

echo ""
echo "================================================================================"
echo " AegisMCP is READY"
echo "================================================================================"
# Print each person's dashboard URL + password + MCP token from server log lines
grep "dashboard http://" /tmp/aegis.log || cat /tmp/aegis.log
echo "================================================================================"
echo ""

# open http://127.0.0.1:8787/ (macOS open)
echo "Opening http://127.0.0.1:${PORT}/ in browser..."
open "http://127.0.0.1:${PORT}/" 2>/dev/null || true

echo "Server running in background (PID $SERVER_PID). Press Ctrl+C to exit."
wait "$SERVER_PID"
