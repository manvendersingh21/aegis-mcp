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

# Clean up servers on exit
SERVER_PID=""
BROKER_PID=""
cleanup() {
  if [ -n "$BROKER_PID" ] && kill -0 "$BROKER_PID" 2>/dev/null; then
    kill "$BROKER_PID" 2>/dev/null || true
  fi
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    echo ""
    echo "Stopping AegisMCP server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

echo "Building target/release/aegis and target/release/aegis-broker..."
cargo build --release -p aegis -p aegis-broker

# Fresh local hot-path db; history is rehydrated from Supabase.
rm -f aegis.db aegis.db-shm aegis.db-wal

if [ "${AEGIS_OFFLINE:-0}" != "1" ]; then
  # Local model on the GPU box, loopback only (never the public tunnel).
  if ! curl -s -o /dev/null --max-time 3 http://127.0.0.1:11436/v1/models; then
    echo "Opening ssh loopback forward to the GPU box (127.0.0.1:11436)..."
    ssh -fN -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 \
      -L 127.0.0.1:11436:127.0.0.1:11436 cis-a6000 || echo "WARN: model forward failed; LLM layers will fall back"
  fi
  # The broker is the only process with real provider keys (.env.broker).
  echo "Starting target/release/aegis-broker on 127.0.0.1:8788 (logs to /tmp/aegis-broker.log)..."
  : > /tmp/aegis-broker.log
  target/release/aegis-broker > /tmp/aegis-broker.log 2>&1 &
  BROKER_PID=$!
  i=0
  until curl -s -o /dev/null "http://127.0.0.1:8788/admin/secrets"; do
    i=$((i + 1))
    if [ "$i" -gt 50 ] || ! kill -0 "$BROKER_PID" 2>/dev/null; then
      echo "ERROR: aegis-broker did not start. Logs:" >&2
      cat /tmp/aegis-broker.log >&2
      exit 1
    fi
    sleep 0.2
  done
fi

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
