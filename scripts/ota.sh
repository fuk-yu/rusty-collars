#!/usr/bin/env bash
# Must be run inside `nix develop` (typically via direnv).
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"

HOST="${REMAINING_ARGS[0]:?Usage: $0 --target <target> <host-or-ip>}"
FW_BIN="$project_dir/target/$TARGET_TRIPLE/release/rusty-collars.bin"

if [[ "$OPT_CLEAN" == true ]]; then
  cargo +esp clean --target "$TARGET_TRIPLE"
  # See build-bin.sh for the rationale: stale esp-idf-sys build script
  # embeds clang-sys's libclang lookup from a previous LIBCLANG_PATH.
  rm -rf target/release/build/esp-idf-sys-* target/debug/build/esp-idf-sys-*
fi

setup_build_env "$project_dir" "$TARGET_NAME"

# Build the Vite frontend (produces frontend/dist/index.html for embedding)
(cd "$project_dir/frontend" && npm install --prefer-offline --no-audit && npm run build)

cargo +esp build --release --target "$TARGET_TRIPLE" $CARGO_FEATURES

# Convert ELF to flashable binary (use esptool.py — espflash's conversion is broken for P4)
esptool="$(find_esptool_py)"
"$esptool" --chip "$TARGET_CHIP" elf2image --output "$FW_BIN" "$project_dir/$TARGET_BINARY"

SIZE=$(stat -c%s "$FW_BIN")
echo "Uploading $FW_BIN ($(numfmt --to=iec "$SIZE")) to $HOST..."

curl -X POST \
  --data-binary "@$FW_BIN" \
  -H "Content-Type: application/octet-stream" \
  --max-time 120 \
  --progress-bar \
  "http://$HOST/ota"

echo ""
echo "Device will reboot with new firmware."
