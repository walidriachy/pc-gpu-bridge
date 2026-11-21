#!/usr/bin/env bash
# Start the real Python agent on a temp root, pair with it, and run the Rust end-to-end test against it.
set -euo pipefail
PY=${PYTHON:-agent/.venv/bin/python}
ROOT=$(mktemp -d); ROOT=$(cd "$ROOT" && pwd -P)
LOG="$ROOT/../pcbridge-agent-e2e.log"
"$PY" -m pcbridge_agent --root "$ROOT" --port 18765 --host 127.0.0.1 >"$LOG" 2>&1 &
APID=$!
trap 'kill $APID 2>/dev/null || true' EXIT
for _ in $(seq 1 40); do curl -sf http://127.0.0.1:18765/health >/dev/null && break; sleep 1; done
CODE=$("$PY" -c "import json;print(json.load(open('$ROOT/config/pairing.json'))['code'])")
TOKEN=$(curl -sf -X POST http://127.0.0.1:18765/pair -H 'content-type: application/json' \
  -d "{\"code\":\"$CODE\",\"device_name\":\"e2e\"}" | "$PY" -c 'import json,sys;print(json.load(sys.stdin)["token"])')
export PCBRIDGE_E2E_URL=http://127.0.0.1:18765 PCBRIDGE_E2E_TOKEN=$TOKEN PCBRIDGE_E2E_ROOT=$ROOT PCBRIDGE_NO_NOTIFY=1
cargo test -p bridge-core --test e2e_mock_agent real_python -- --nocapture
