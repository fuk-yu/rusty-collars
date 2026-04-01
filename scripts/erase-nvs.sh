#!/usr/bin/env bash
set -euo pipefail

# Erase the NVS partition on any supported target board.
# NVS is at offset 0x9000, size 0x6000 across all partition tables.
#
# Usage: ./scripts/erase-nvs.sh --target <esp32|esp32c6|esp32p4|esp32p4-wifi>

cd "$(dirname "$0")/.."
source scripts/target-info.sh
parse_target_arg "$@"

NVS_OFFSET=0x9000
NVS_SIZE=0x6000

echo "Erasing NVS partition on ${TARGET_NAME} (offset=${NVS_OFFSET}, size=${NVS_SIZE})..."

PYTHON="$(find_idf_python "$PWD")"
"$PYTHON" -m esptool --chip "$TARGET_CHIP" erase_region "$NVS_OFFSET" "$NVS_SIZE"

echo "NVS erased. Power-cycle the board to boot with default settings."
