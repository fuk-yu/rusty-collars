#!/usr/bin/env bash
# Must be run inside `nix develop` (typically via direnv).
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"
setup_build_env "$project_dir" "$TARGET_NAME"

PORT=18080
PASS=0
FAIL=0

pass() { PASS=$((PASS+1)); echo "  PASS: $1"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $1 - $2"; }

ws_send_recv() {
    # Send a JSON message over WebSocket and capture the expected response.
    # websocat opens a connection, receives initial state+debug messages,
    # then we send our message and look for the expected response type.
    local msg="$1" expected_type="$2" timeout_s="${3:-5}"
    # Use a subshell: sleep briefly to let initial messages arrive, then send ours
    (sleep 0.5; echo "$msg") | \
        timeout "$timeout_s" websocat "ws://localhost:$PORT/ws" 2>/dev/null | \
        grep -m1 "\"type\":\"$expected_type\"" || true
}

echo "=== QEMU Integration Tests ($TARGET_NAME) ==="
echo ""

case "$TARGET_NAME" in
    esp32) ;;
    *)
        echo "QEMU integration tests only supported for esp32" >&2
        exit 1
        ;;
esac

# --- Build + create flash image ---
echo "[setup] Building firmware..."
if ! cargo +esp build --release 2>&1 | tail -1 | grep -q "Finished"; then
    echo "Build failed!"
    exit 1
fi

echo "[setup] Creating flash image..."
espflash save-image --chip "$TARGET_CHIP" --merge \
    "$project_dir/$TARGET_BINARY" \
    target/flash_image_test.bin 2>/dev/null

# --- Start QEMU ---
echo "[setup] Starting QEMU..."
QEMU_LOG=$(mktemp)
qemu-system-xtensa -nographic -machine esp32 \
    -drive "file=target/flash_image_test.bin,if=mtd,format=raw" \
    -nic "user,model=open_eth,hostfwd=tcp:127.0.0.1:${PORT}-:80" \
    -serial mon:stdio > "$QEMU_LOG" 2>&1 &
QEMU_PID=$!

cleanup() {
    kill "$QEMU_PID" 2>/dev/null
    wait "$QEMU_PID" 2>/dev/null || true
    rm -f "$QEMU_LOG"
}
trap cleanup EXIT

# Wait for server
echo "[setup] Waiting for server..."
for i in $(seq 1 20); do
    if grep -q "picoserve listening on port 80" "$QEMU_LOG" 2>/dev/null; then
        break
    fi
    sleep 1
done

if ! grep -q "picoserve listening on port 80" "$QEMU_LOG"; then
    echo "Server failed to start!"
    cat "$QEMU_LOG" | sed 's/\x1b\[[0-9;]*m//g' | tail -20
    exit 1
fi
echo "[setup] Server ready."
echo ""

# === HTTP Tests ===

echo "[HTTP]"

# Test: GET / returns 200 with gzipped HTML
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://localhost:$PORT/" 2>/dev/null)
if [ "$HTTP_CODE" = "200" ]; then
    pass "GET / returns 200"
else
    fail "GET / returns 200" "got $HTTP_CODE"
fi

# Test: Content-Encoding is gzip
ENCODING=$(curl -s -D- -o /dev/null "http://localhost:$PORT/" 2>/dev/null | grep -i "content-encoding" | tr -d '\r')
if echo "$ENCODING" | grep -qi "gzip"; then
    pass "GET / Content-Encoding: gzip"
else
    fail "GET / Content-Encoding: gzip" "got '$ENCODING'"
fi

# Test: favicon
FAVICON_CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://localhost:$PORT/favicon.ico" 2>/dev/null)
if [ "$FAVICON_CODE" = "200" ]; then
    pass "GET /favicon.ico returns 200"
else
    fail "GET /favicon.ico returns 200" "got $FAVICON_CODE"
fi

# Test: 404 for unknown path
NOT_FOUND=$(curl -s -o /dev/null -w "%{http_code}" "http://localhost:$PORT/nonexistent" 2>/dev/null)
if [ "$NOT_FOUND" = "404" ]; then
    pass "GET /nonexistent returns 404"
else
    fail "GET /nonexistent returns 404" "got $NOT_FOUND"
fi

echo ""

# === WebSocket Tests ===

echo "[WebSocket]"

# Test: Ping/Pong
PONG=$(ws_send_recv '{"type":"ping","nonce":42}' "pong")
if echo "$PONG" | grep -q '"nonce":42'; then
    pass "ping/pong with nonce"
else
    fail "ping/pong with nonce" "got '$PONG'"
fi

# Test: Pong contains uptime and heap
if echo "$PONG" | grep -q '"server_uptime_s"' && echo "$PONG" | grep -q '"free_heap_bytes"'; then
    pass "pong contains uptime + heap"
else
    fail "pong contains uptime + heap" "missing fields"
fi

# Test: Pong contains connected_clients
if echo "$PONG" | grep -q '"connected_clients"'; then
    pass "pong contains connected_clients"
else
    fail "pong contains connected_clients" "missing field"
fi

# Test: Get initial state (sent on WS connect)
STATE=$(echo "" | timeout 3 websocat -n1 "ws://localhost:$PORT/ws" 2>/dev/null | head -1)
if echo "$STATE" | grep -q '"type":"state"'; then
    pass "initial state sent on connect"
else
    fail "initial state sent on connect" "got '$STATE'"
fi

# Test: State contains expected fields
if echo "$STATE" | grep -q '"collars"' && echo "$STATE" | grep -q '"presets"' && echo "$STATE" | grep -q '"app_version"'; then
    pass "state has collars, presets, app_version"
else
    fail "state has collars, presets, app_version" "missing fields"
fi

# Test: Add collar - look for state that contains our collar
ADD_RESULT=$( (sleep 0.5; echo '{"type":"add_collar","name":"TestCollar","collar_id":4660,"channel":0}') | \
    timeout 5 websocat "ws://localhost:$PORT/ws" 2>/dev/null | \
    grep -m1 "TestCollar" || true )
if echo "$ADD_RESULT" | grep -q '"TestCollar"'; then
    pass "add_collar creates collar"
else
    fail "add_collar creates collar" "collar not in state"
fi

# Test: Add duplicate collar fails
DUP_RESULT=$(ws_send_recv '{"type":"add_collar","name":"TestCollar","collar_id":9999,"channel":0}' "error")
if echo "$DUP_RESULT" | grep -q "already exists"; then
    pass "add duplicate collar returns error"
else
    fail "add duplicate collar returns error" "got '$DUP_RESULT'"
fi

# Test: Add collar with invalid channel
BAD_CH=$(ws_send_recv '{"type":"add_collar","name":"BadCh","collar_id":1111,"channel":5}' "error")
if echo "$BAD_CH" | grep -q "invalid channel"; then
    pass "add collar with bad channel returns error"
else
    fail "add collar with bad channel returns error" "got '$BAD_CH'"
fi

# Test: Export
EXPORT=$(ws_send_recv '{"type":"export"}' "export_data")
if echo "$EXPORT" | grep -q '"collars"' && echo "$EXPORT" | grep -q '"presets"'; then
    pass "export returns collars + presets"
else
    fail "export returns collars + presets" "missing fields"
fi

# Test: Get device settings
SETTINGS=$(ws_send_recv '{"type":"get_device_settings"}' "device_settings")
if echo "$SETTINGS" | grep -q '"led_pin"' && echo "$SETTINGS" | grep -q '"rf_tx_pin"'; then
    pass "get_device_settings returns pin config"
else
    fail "get_device_settings returns pin config" "got '$SETTINGS'"
fi

# Test: Delete collar - after delete, the state should not contain TestCollar.
# Send delete, then ping to get fresh state.
DEL_RESULT=$( (sleep 0.5; echo '{"type":"delete_collar","name":"TestCollar"}'; sleep 0.3; echo '{"type":"ping","nonce":999}') | \
    timeout 5 websocat "ws://localhost:$PORT/ws" 2>/dev/null | \
    grep -m1 '"nonce":999' || true )
# The pong response arrives after delete processed. Check latest state.
AFTER_DEL=$( (sleep 0.3; echo '{"type":"export"}') | \
    timeout 5 websocat "ws://localhost:$PORT/ws" 2>/dev/null | \
    grep -m1 '"export_data"' || true )
if echo "$AFTER_DEL" | grep -q '"collars"' && ! echo "$AFTER_DEL" | grep -q '"TestCollar"'; then
    pass "delete_collar removes collar"
else
    fail "delete_collar removes collar" "collar still present"
fi

# Test: Delete nonexistent collar
DEL_MISSING=$(ws_send_recv '{"type":"delete_collar","name":"Ghost"}' "error")
if echo "$DEL_MISSING" | grep -q "Unknown collar"; then
    pass "delete nonexistent collar returns error"
else
    fail "delete nonexistent collar returns error" "got '$DEL_MISSING'"
fi

# Test: Invalid JSON
INVALID=$(ws_send_recv 'not json at all' "error")
if echo "$INVALID" | grep -q "Invalid message"; then
    pass "invalid JSON returns error"
else
    fail "invalid JSON returns error" "got '$INVALID'"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
