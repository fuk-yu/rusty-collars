#!/usr/bin/env bash
set -euo pipefail

# Build and flash the esp_hosted slave firmware for the companion ESP32-C6
# on the Waveshare ESP32-P4-WIFI6-POE-ETH board.
#
# Prerequisites:
#   1. ESP-IDF environment must be active
#   2. Connect a USB-to-serial adapter to the C6 UART pads on the board
#      (C6_U0RXD, C6_U0TXD, GND)
#   3. Put the C6 into download mode:
#      - Short C6_IO9 to GND
#      - Hold P4 BOOT button
#      - Press and release RESET
#      - Release C6_IO9
#   4. Release P4 BOOT after flashing completes
#
# Usage: ./scripts/flash-c6-slave.sh <serial-port> [--monitor]
#   e.g. ./scripts/flash-c6-slave.sh /dev/ttyUSB0
#        ./scripts/flash-c6-slave.sh /dev/ttyUSB0 --monitor

cd "$(dirname "$0")/.."
PROJECT_DIR="$PWD"

C6_SERIAL_PORT="${1:?Usage: $0 <serial-port> [--monitor]  (e.g. /dev/ttyUSB0)}"
MONITOR="${2:-}"

source "$PROJECT_DIR/scripts/esp-idf-env.sh"

# The slave source is bundled inside the esp_hosted component
SLAVE_SRC=$(find "$PROJECT_DIR/target" -path "*/managed_components/espressif__esp_hosted/slave" -type d | head -1)
if [ -z "$SLAVE_SRC" ]; then
    echo "ERROR: esp_hosted slave source not found. Build the P4-WiFi firmware first:" >&2
    echo "  scripts/select-target.sh esp32p4-wifi && cargo build --release --features p4-wifi" >&2
    exit 1
fi

echo "=== esp_hosted slave source: $SLAVE_SRC ==="

BUILD_DIR="$PROJECT_DIR/target/c6-slave-build"
if [ ! -f "$BUILD_DIR/build/flasher_args.json" ]; then
    rm -rf "$BUILD_DIR"
    cp -r "$SLAVE_SRC" "$BUILD_DIR"
    cd "$BUILD_DIR"

    # SDIO transport + 8MB flash (Waveshare C6-MINI-1U has 8MB)
    cat > sdkconfig.defaults <<'EOF'
CONFIG_ESP_SDIO_HOST_INTERFACE=y
CONFIG_ESPTOOLPY_FLASHSIZE_8MB=y
EOF

    idf.py set-target esp32c6
    idf.py build
else
    echo "=== Using existing build in $BUILD_DIR (delete to rebuild) ==="
    cd "$BUILD_DIR"
fi

echo ""
echo "=== Flashing C6 slave firmware via $C6_SERIAL_PORT ==="

FLASH_ARGS="flash"
if [ "$MONITOR" = "--monitor" ]; then
    FLASH_ARGS="flash monitor"
fi

idf.py -p "$C6_SERIAL_PORT" $FLASH_ARGS

echo ""
echo "=== Done! Reset the board to test WiFi. ==="
