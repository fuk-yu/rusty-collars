#!/usr/bin/env bash
# Build firmware and produce a .bin file suitable for OTA upload (web UI or curl).
# Output: target/<triple>/release/rusty-collars.bin
# Must be run inside `nix develop` (typically via direnv).
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"

if [[ "$OPT_CLEAN" == true ]]; then
  cargo +esp clean --target "$TARGET_TRIPLE"
  # cargo clean --target only purges target/$TRIPLE/, leaving the host
  # build script binary at target/release/build/esp-idf-sys-*/ behind.
  # That binary embeds clang-sys's libclang lookup from when it was
  # last compiled and won't pick up a new LIBCLANG_PATH on its own
  # (cargo rerun-if-env-changed re-executes, never recompiles). Wipe
  # it so the build script is rebuilt against the current /nix/store
  # toolchain instead of a stale path.
  rm -rf target/release/build/esp-idf-sys-* target/debug/build/esp-idf-sys-*
fi

setup_build_env "$project_dir" "$TARGET_NAME"

# Build the Vite frontend (produces frontend/dist/index.html for embedding)
(cd "$project_dir/frontend" && npm install --prefer-offline --no-audit && npm run build)

cargo +esp build --release --target "$TARGET_TRIPLE" $CARGO_FEATURES

FW_BIN="$project_dir/target/$TARGET_TRIPLE/release/rusty-collars.bin"
esptool="$(find_esptool_py)"
"$esptool" --chip "$TARGET_CHIP" elf2image --output "$FW_BIN" "$project_dir/$TARGET_BINARY"

SIZE=$(stat -c%s "$FW_BIN")
echo ""
echo "Firmware binary: $FW_BIN ($(numfmt --to=iec "$SIZE"))"
echo "Upload via: curl -X POST --data-binary @$FW_BIN http://<device-ip>/ota"
echo "Or use the web UI Settings > Firmware Update (OTA)"
