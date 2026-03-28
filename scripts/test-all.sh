#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"

PASS=0
FAIL=0
SKIP=0

result() {
    local name="$1" status="$2"
    case "$status" in
        pass) PASS=$((PASS+1)); echo "  PASS: $name" ;;
        fail) FAIL=$((FAIL+1)); echo "  FAIL: $name" ;;
        skip) SKIP=$((SKIP+1)); echo "  SKIP: $name" ;;
    esac
}

echo "=== rusty-collars test suite ==="
echo ""

# --- Unit tests (host) ---
echo "[1/4] Unit tests (host, rusty-collars-core)..."
if cargo test -p rusty-collars-core --target x86_64-unknown-linux-gnu 2>&1 | tail -3 | grep -q "test result: ok"; then
    result "unit tests" pass
else
    result "unit tests" fail
fi

# --- ESP32 build ---
echo "[2/4] ESP32 build..."
activate_target "$project_dir" esp32
if cargo +esp clean 2>/dev/null && cargo +esp build --release 2>&1 | tail -1 | grep -q "Finished"; then
    result "esp32 build" pass

    # --- ESP32 QEMU integration tests ---
    echo "[3/4] ESP32 QEMU integration tests..."
    if bash ./scripts/integration-test.sh --target esp32 2>&1 | tee /dev/stderr | tail -1 | grep -q "0 failed"; then
        result "esp32 qemu integration" pass
    else
        result "esp32 qemu integration" fail
    fi
else
    result "esp32 build" fail
    result "esp32 qemu" skip
fi

# --- ESP32-C6 build ---
echo "[4/4] ESP32-C6 build..."
activate_target "$project_dir" esp32c6
# C6 needs RISC-V tools. Check if riscv32-esp-elf-gcc is available.
if command -v riscv32-esp-elf-gcc &>/dev/null || find "$project_dir/.embuild" -name "riscv32-esp-elf-gcc" 2>/dev/null | grep -q .; then
    if ESP_IDF_TOOLS_INSTALL_DIR=fromenv cargo clean 2>/dev/null && ESP_IDF_TOOLS_INSTALL_DIR=fromenv cargo build --release 2>&1 | tail -1 | grep -q "Finished"; then
        result "esp32c6 build" pass
    else
        result "esp32c6 build" fail
    fi
else
    echo "  riscv32-esp-elf-gcc not found. Run: espup install --targets esp32c6"
    result "esp32c6 build" skip
fi
# No QEMU for C6 yet
result "esp32c6 qemu" skip

# Restore ESP32 as default
activate_target "$project_dir" esp32

echo ""
echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="
[ "$FAIL" -eq 0 ] || exit 1
