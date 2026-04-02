#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"
. "$project_dir/scripts/prepare-toolchain-env.sh"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"

HOST="${REMAINING_ARGS[0]:?Usage: $0 --target <target> <host-or-ip>}"
FW_BIN="$project_dir/target/$TARGET_TRIPLE/release/rusty-collars.bin"

run_in_env() {
  if [[ -n "${DIRENV_DIR:-}" ]]; then
    "$@"
  else
    direnv exec "$project_dir" "$@"
  fi
}

run_in_env bash -lc 'cargo +esp --version >/dev/null 2>&1 || { echo "Missing repo-local ESP toolchain. Run ./scripts/bootstrap-toolchain.sh" >&2; exit 1; }'

if [[ "$OPT_CLEAN" == true ]]; then
  run_in_env cargo +esp clean --target "$TARGET_TRIPLE"
fi

setup_build_env "$project_dir" "$TARGET_NAME"

# Build the Vite frontend (produces frontend/dist/index.html for embedding)
(cd "$project_dir/frontend" && npm install --prefer-offline --no-audit && npm run build)

run_in_env cargo +esp build --release --target "$TARGET_TRIPLE" $CARGO_FEATURES

# Convert ELF to flashable binary (use esptool.py — espflash's conversion is broken for P4)
idf_python="$(find_idf_python "$project_dir")"
esptool="$(find_esptool_py "$project_dir")"
run_in_env "$idf_python" "$esptool" --chip "$TARGET_CHIP" elf2image --output "$FW_BIN" "$project_dir/$TARGET_BINARY"

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
