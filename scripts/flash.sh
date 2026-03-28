#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"
activate_target "$project_dir" "$TARGET_NAME"

run_in_env() {
  if [[ -n "${DIRENV_DIR:-}" ]]; then
    "$@"
  else
    direnv exec "$project_dir" "$@"
  fi
}

run_in_env bash -lc 'cargo +esp --version >/dev/null 2>&1 || { echo "Missing repo-local ESP toolchain. Run ./scripts/bootstrap-toolchain.sh" >&2; exit 1; }'

if [[ "$OPT_CLEAN" == true ]]; then
  run_in_env cargo +esp clean
fi

run_in_env cargo +esp build --release

idf_build_dir=$(echo "$project_dir"/$IDF_BUILD_DIR)
bootloader_bin="$idf_build_dir/bootloader/bootloader.bin"

if [[ -n "$ESPFLASH_FLASH_ARGS" ]]; then
  # Targets where espflash's ELF conversion is broken (e.g. P4 rev <3.0):
  # use esptool.py for ELF→bin, espflash write-bin for fast transfer
  esptool=$(find "$project_dir/.embuild/espressif/python_env" -name 'esptool.py' -path '*/bin/*' | head -1)
  app_bin="$project_dir/target/$TARGET_TRIPLE/release/rusty-collars.bin"
  run_in_env "$esptool" --chip "$TARGET_CHIP" elf2image --output "$app_bin" "$project_dir/$TARGET_BINARY"

  if [[ "$OPT_BOOTLOADER" == true ]]; then
    run_in_env espflash write-bin $ESPFLASH_FLASH_ARGS "$BOOTLOADER_OFFSET" "$bootloader_bin"
    run_in_env espflash write-bin $ESPFLASH_FLASH_ARGS 0x8000 "$idf_build_dir/partition_table/partition-table.bin"
  fi
  run_in_env espflash write-bin $ESPFLASH_FLASH_ARGS 0x20000 "$app_bin"
else
  # Normal targets: espflash handles everything
  if [[ "$OPT_BOOTLOADER" == true ]]; then
    run_in_env espflash flash --baud 2000000 --bootloader "$bootloader_bin" "$project_dir/$TARGET_BINARY"
  else
    run_in_env espflash flash --baud 2000000 "$project_dir/$TARGET_BINARY"
  fi
fi

if [[ "$OPT_MONITOR" == true ]]; then
  run_in_env espflash monitor $ESPFLASH_MONITOR_ARGS
fi
