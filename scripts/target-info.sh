#!/usr/bin/env bash
# Shared helper: resolves --target <name> into build triple, binary path, and chip name.
# Source this file after parsing --target from your script's arguments.
#
# Usage:
#   source "$(dirname "$0")/target-info.sh"
#   resolve_target "esp32p4"
#
# After calling resolve_target, these variables are set:
#   TARGET_NAME   - e.g. "esp32p4"
#   TARGET_TRIPLE - e.g. "riscv32imafc-esp-espidf"
#   TARGET_BINARY - e.g. "target/riscv32imafc-esp-espidf/release/rusty-collars"
#   TARGET_CHIP   - e.g. "esp32p4" (espflash chip name)
#   ESPFLASH_EXTRA_ARGS - extra args for espflash (e.g. "--no-stub" for P4)

SUPPORTED_TARGETS="esp32 esp32c6 esp32p4"

resolve_target() {
  TARGET_NAME="${1:?Usage: resolve_target <esp32|esp32c6|esp32p4>}"

  ESPFLASH_FLASH_ARGS=""
  ESPFLASH_MONITOR_ARGS=""

  case "$TARGET_NAME" in
    esp32)
      TARGET_TRIPLE="xtensa-esp32-espidf"
      TARGET_CHIP="esp32"
      BOOTLOADER_OFFSET=0x1000
      PARTITION_TABLE="partitions-4mb.csv"
      ;;
    esp32c6)
      TARGET_TRIPLE="riscv32imac-esp-espidf"
      TARGET_CHIP="esp32c6"
      BOOTLOADER_OFFSET=0x0
      PARTITION_TABLE="partitions-8mb.csv"
      ;;
    esp32p4)
      TARGET_TRIPLE="riscv32imafc-esp-espidf"
      TARGET_CHIP="esp32p4"
      # P4 rev <3.0: espflash stub doesn't work (espflash #1013),
      # and espflash's ELF-to-image conversion is broken for P4 —
      # we use esptool.py elf2image + espflash write-bin instead
      ESPFLASH_FLASH_ARGS="--no-stub"
      ESPFLASH_MONITOR_ARGS="--no-stub"
      BOOTLOADER_OFFSET=0x2000
      PARTITION_TABLE="partitions-16mb.csv"
      ;;
    *)
      echo "Unknown target: $TARGET_NAME (supported: $SUPPORTED_TARGETS)" >&2
      exit 1
      ;;
  esac

  TARGET_BINARY="target/${TARGET_TRIPLE}/release/rusty-collars"
  IDF_BUILD_DIR="target/${TARGET_TRIPLE}/release/build/esp-idf-sys-*/out/build"
}

# Copies the matching .cargo/config-<target>.toml and sdkconfig.defaults.<target>
# into the active locations.
activate_target() {
  local project_dir="${1:?activate_target requires project dir}"
  resolve_target "${2:?activate_target requires target name}"

  cp "$project_dir/.cargo/config-${TARGET_NAME}.toml" "$project_dir/.cargo/config.toml"
  cp "$project_dir/sdkconfig.defaults.${TARGET_NAME}" "$project_dir/sdkconfig.defaults"
  cp "$project_dir/${PARTITION_TABLE}" "$project_dir/partitions.csv"
  cat > "$project_dir/sdkconfig.defaults.partitions" <<EOF
CONFIG_PARTITION_TABLE_CUSTOM=y
CONFIG_PARTITION_TABLE_CUSTOM_FILENAME="$project_dir/partitions.csv"
EOF

  # espflash needs to know the partition table for OTA
  cat > "$project_dir/espflash.toml" <<EOF
[idf_format_args]
partition_table = "partitions.csv"
EOF

  # Ensure ESP-IDF checkout matches the version in Cargo.toml
  _ensure_idf_version "$project_dir"
}

_ensure_idf_version() {
  local project_dir="$1"
  local wanted
  wanted=$(grep 'esp_idf_version' "$project_dir/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
  [ -n "$wanted" ] || return 0

  # Find the ESP-IDF checkout (directory name may not match the actual tag)
  local idf_dir
  for idf_dir in "$project_dir"/.embuild/espressif/esp-idf/v5.*; do
    [ -d "$idf_dir/.git" ] || continue
    local current
    current=$(git -C "$idf_dir" describe --tags --exact-match 2>/dev/null || git -C "$idf_dir" describe --tags 2>/dev/null || echo "unknown")
    if [ "$current" != "$wanted" ]; then
      echo "ESP-IDF: $current -> $wanted"
      git -C "$idf_dir" fetch --tags --quiet
      git -C "$idf_dir" checkout "$wanted" --quiet
      git -C "$idf_dir" submodule update --init --recursive --quiet
    fi
    return 0
  done
}

find_idf_python() {
  local project_dir="${1:?find_idf_python requires project dir}"

  if [ -n "${IDF_PYTHON_ENV_PATH:-}" ] && [ -x "${IDF_PYTHON_ENV_PATH}/bin/python" ]; then
    printf '%s\n' "${IDF_PYTHON_ENV_PATH}/bin/python"
    return 0
  fi

  local python_bin
  while IFS= read -r python_bin; do
    printf '%s\n' "$python_bin"
    return 0
  done < <(find "$project_dir/.embuild/espressif/python_env" -type f -path '*/bin/python' | sort)

  echo "Missing ESP-IDF python under $project_dir/.embuild/espressif/python_env" >&2
  return 1
}

find_esptool_py() {
  local project_dir="${1:?find_esptool_py requires project dir}"
  local esptool

  while IFS= read -r esptool; do
    printf '%s\n' "$esptool"
    return 0
  done < <(find "$project_dir/.embuild/espressif/python_env" -type f -path '*/bin/esptool.py' | sort)

  echo "Missing esptool.py under $project_dir/.embuild/espressif/python_env" >&2
  return 1
}

# Parses --target <name> and optional flags from arguments. Dies if --target is missing.
# Remaining args are placed in REMAINING_ARGS.
# Sets: TARGET_NAME, TARGET_TRIPLE, TARGET_BINARY, TARGET_CHIP, OPT_CLEAN
parse_target_arg() {
  TARGET_NAME=""
  OPT_CLEAN=false
  OPT_BOOTLOADER=false
  OPT_MONITOR=false
  REMAINING_ARGS=()

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --target)
        TARGET_NAME="${2:?--target requires a value ($SUPPORTED_TARGETS)}"
        shift 2
        ;;
      --clean)
        OPT_CLEAN=true
        shift
        ;;
      --bootloader)
        OPT_BOOTLOADER=true
        shift
        ;;
      --monitor)
        OPT_MONITOR=true
        shift
        ;;
      *)
        REMAINING_ARGS+=("$1")
        shift
        ;;
    esac
  done

  if [[ -z "$TARGET_NAME" ]]; then
    echo "Error: --target is required ($SUPPORTED_TARGETS)" >&2
    exit 1
  fi

  resolve_target "$TARGET_NAME"
}
