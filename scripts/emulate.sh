#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"
activate_target "$project_dir" "$TARGET_NAME"

HOST_PORT="${REMAINING_ARGS[0]:-8080}"
FLASH_IMAGE="target/flash_image.bin"

if [ ! -f "$project_dir/$TARGET_BINARY" ]; then
    echo "Binary not found. Building..."
    cargo +esp build --release
fi

echo "Creating merged flash image..."
espflash save-image --chip "$TARGET_CHIP" --merge "$project_dir/$TARGET_BINARY" "$FLASH_IMAGE"

case "$TARGET_NAME" in
    esp32)
        QEMU_CMD=(qemu-system-xtensa -machine esp32)
        ;;
    *)
        echo "QEMU emulation not supported for $TARGET_NAME" >&2
        exit 1
        ;;
esac

echo "Starting QEMU $TARGET_NAME (port $HOST_PORT -> ESP:80)..."
echo "  Web UI: http://localhost:$HOST_PORT"
echo "  Press Ctrl+A then X to exit QEMU"
echo ""

"${QEMU_CMD[@]}" \
    -nographic \
    -drive "file=$FLASH_IMAGE,if=mtd,format=raw" \
    -nic "user,model=open_eth,hostfwd=tcp:127.0.0.1:${HOST_PORT}-:80" \
    -serial mon:stdio
