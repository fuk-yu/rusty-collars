#!/usr/bin/env bash
# Must be run inside `nix develop` (typically via direnv).
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"

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

idf_build_dir=$(echo "$project_dir"/$IDF_BUILD_DIR)
bootloader_bin="$idf_build_dir/bootloader/bootloader.bin"

if [[ -n "$ESPFLASH_FLASH_ARGS" ]]; then
  # Targets where espflash's ELF conversion is broken (e.g. P4 rev <3.0):
  # use esptool.py for ELF→bin, espflash write-bin for fast transfer
  esptool="$(find_esptool_py)"
  app_bin="$project_dir/target/$TARGET_TRIPLE/release/rusty-collars.bin"
  "$esptool" --chip "$TARGET_CHIP" elf2image --output "$app_bin" "$project_dir/$TARGET_BINARY"

  # Erase otadata so the bootloader boots from ota_0 (where we flash).
  # Without this, a previous OTA update may have switched to ota_1,
  # causing the bootloader to ignore the freshly flashed ota_0 image.
  # espflash erase-region requires the RAM stub which doesn't work on
  # some chips (e.g. P4 rev <3.0), so we use esptool.py instead.
  "$esptool" --chip "$TARGET_CHIP" --no-stub erase_region 0xf000 0x2000

  if [[ "$OPT_BOOTLOADER" == true ]]; then
    espflash write-bin $ESPFLASH_FLASH_ARGS "$BOOTLOADER_OFFSET" "$bootloader_bin"
    espflash write-bin $ESPFLASH_FLASH_ARGS 0x8000 "$idf_build_dir/partition_table/partition-table.bin"
  fi
  espflash write-bin $ESPFLASH_FLASH_ARGS 0x20000 "$app_bin"
else
  # Normal targets: espflash handles everything
  if [[ "$OPT_BOOTLOADER" == true ]]; then
    espflash flash --baud 2000000 --bootloader "$bootloader_bin" "$project_dir/$TARGET_BINARY"
  else
    espflash flash --baud 2000000 "$project_dir/$TARGET_BINARY"
  fi
fi

if [[ "$OPT_MONITOR" == true ]]; then
  espflash monitor $ESPFLASH_MONITOR_ARGS
fi
