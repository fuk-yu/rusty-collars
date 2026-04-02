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

target_info_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$target_info_dir/esp-idf-env.sh"

SUPPORTED_TARGETS="esp32 esp32c6 esp32p4 esp32p4-wifi"

resolve_target() {
  TARGET_NAME="${1:?Usage: resolve_target <esp32|esp32c6|esp32p4|esp32p4-wifi>}"

  ESPFLASH_FLASH_ARGS=""
  ESPFLASH_MONITOR_ARGS=""
  CARGO_FEATURES=""
  CARGO_CONFIG_SOURCE=""

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
    esp32p4-wifi)
      TARGET_TRIPLE="riscv32imafc-esp-espidf"
      TARGET_CHIP="esp32p4"
      ESPFLASH_FLASH_ARGS="--no-stub"
      ESPFLASH_MONITOR_ARGS="--no-stub"
      BOOTLOADER_OFFSET=0x2000
      PARTITION_TABLE="partitions-16mb.csv"
      CARGO_FEATURES="--features p4-wifi"
      CARGO_CONFIG_SOURCE="esp32p4"
      ;;
    *)
      echo "Unknown target: $TARGET_NAME (supported: $SUPPORTED_TARGETS)" >&2
      exit 1
      ;;
  esac

  TARGET_BINARY="target/${TARGET_TRIPLE}/release/rusty-collars"
  IDF_BUILD_DIR="target/${TARGET_TRIPLE}/release/build/esp-idf-sys-*/out/build"
}

# Sets environment variables for an isolated, non-interfering build of the given target.
# Multiple targets can build in parallel — no shared files are overwritten.
setup_build_env() {
  local project_dir="${1:?setup_build_env requires project dir}"
  resolve_target "${2:?setup_build_env requires target name}"

  # Ensure ESP-IDF checkout matches the version in Cargo.toml
  _ensure_idf_version "$project_dir"

  # MCU env var for build.rs WiFi cfg detection
  case "$TARGET_NAME" in
    esp32)                export MCU="" ;;
    esp32c6)              export MCU="esp32c6" ;;
    esp32p4|esp32p4-wifi) export MCU="esp32p4" ;;
  esac

  # Write combined sdkconfig (common + target + partition) to sdkconfig.defaults.
  # IDF resolves partition CSV paths relative to its build dir, so use absolute paths.
  # The resolved IDF sdkconfig is cached per-target in target/<triple>/, so switching
  # targets only triggers an IDF re-merge, not a full rebuild.
  {
    cat "$project_dir/sdkconfig.defaults.common"
    echo ""
    case "$TARGET_NAME" in
      esp32p4-wifi)
        cat "$project_dir/sdkconfig.defaults.esp32p4"
        echo ""
        cat "$project_dir/sdkconfig.defaults.esp32p4-wifi"
        ;;
      *)
        cat "$project_dir/sdkconfig.defaults.$TARGET_NAME"
        ;;
    esac
    echo ""
    echo "CONFIG_PARTITION_TABLE_CUSTOM=y"
    echo "CONFIG_PARTITION_TABLE_CUSTOM_FILENAME=\"$project_dir/$PARTITION_TABLE\""
  } > "$project_dir/sdkconfig.defaults"

  # Clean build cache if toolchain or sdkconfig inputs changed
  _invalidate_build_cache "$project_dir"

  # espflash needs to know the partition table for OTA
  cat > "$project_dir/espflash.toml" <<EOF
[idf_format_args]
partition_table = "$PARTITION_TABLE"
EOF
}

_ensure_idf_version() {
  local project_dir="$1"
  local wanted
  local idf_dir
  local current
  local needs_bootstrap=false

  wanted=$(grep 'esp_idf_version' "$project_dir/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
  [ -n "$wanted" ] || return 0

  idf_dir="$(requested_idf_checkout_path "$project_dir" "$wanted")"
  if [ ! -d "$idf_dir/.git" ]; then
    needs_bootstrap=true
  else
    current="$(git -C "$idf_dir" describe --tags --exact-match 2>/dev/null || true)"
    if [ "$current" != "$wanted" ]; then
      needs_bootstrap=true
    fi
  fi

  if [ "$needs_bootstrap" = true ]; then
    echo "ESP-IDF $wanted not found, running bootstrap-toolchain.sh..."
    "$project_dir/scripts/bootstrap-toolchain.sh"
  fi
}

_target_compiler_path() {
  local project_dir="${1:?_target_compiler_path requires project dir}"
  local bindir
  local compiler

  case "$TARGET_NAME" in
    esp32)
      bindir="$(find_toolchain_bin_dir "$project_dir" xtensa-esp-elf)"
      compiler="$bindir/xtensa-esp-elf-gcc"
      ;;
    esp32c6|esp32p4|esp32p4-wifi)
      bindir="$(find_toolchain_bin_dir "$project_dir" riscv32-esp-elf)"
      compiler="$bindir/riscv32-esp-elf-gcc"
      ;;
    *)
      compiler=""
      ;;
  esac

  if [ -x "$compiler" ]; then
    printf '%s\n' "$compiler"
  else
    printf '%s\n' ""
  fi
}

_build_env_fingerprint() {
  local project_dir="${1:?_build_env_fingerprint requires project dir}"
  local wanted
  local python_env
  local compiler
  local compiler_version
  local sdkconfig_hash

  wanted="$(load_requested_idf_version "$project_dir")"
  python_env="$(find_requested_idf_python_env "$project_dir" "$wanted")"
  compiler="$(_target_compiler_path "$project_dir")"
  compiler_version=""
  if [ -n "$compiler" ]; then
    compiler_version="$("$compiler" --version | head -1)"
  fi

  # Hash the generated sdkconfig.defaults for this target.
  sdkconfig_hash="$(sha256sum "$project_dir/sdkconfig.defaults" 2>/dev/null | cut -d' ' -f1)"

  cat <<EOF
target=$TARGET_NAME
triple=$TARGET_TRIPLE
idf=$wanted
python_env=$python_env
compiler=$compiler
compiler_version=$compiler_version
sdkconfig=$sdkconfig_hash
EOF
}

_invalidate_build_cache() {
  local project_dir="${1:?_invalidate_build_cache requires project dir}"
  local target_dir="$project_dir/target/$TARGET_TRIPLE"
  local fingerprint_path="$target_dir/.esp-build-fingerprint"
  local fingerprint_tmp

  fingerprint_tmp="$(mktemp)"
  _build_env_fingerprint "$project_dir" > "$fingerprint_tmp"

  if [ -f "$fingerprint_path" ] && cmp -s "$fingerprint_tmp" "$fingerprint_path"; then
    rm -f "$fingerprint_tmp"
    return 0
  fi

  echo "Build env changed for $TARGET_NAME; cleaning target/$TARGET_TRIPLE"
  cargo clean --target "$TARGET_TRIPLE"
  mkdir -p "$target_dir"
  mv "$fingerprint_tmp" "$fingerprint_path"
}

find_idf_python() {
  local project_dir="${1:?find_idf_python requires project dir}"
  local wanted
  local python_env

  if [ -n "${IDF_PYTHON_ENV_PATH:-}" ] && [ -x "${IDF_PYTHON_ENV_PATH}/bin/python" ]; then
    printf '%s\n' "${IDF_PYTHON_ENV_PATH}/bin/python"
    return 0
  fi

  wanted="$(load_requested_idf_version "$project_dir")"
  python_env="$(find_requested_idf_python_env "$project_dir" "$wanted")"
  if [ -x "$python_env/bin/python" ]; then
    printf '%s\n' "$python_env/bin/python"
    return 0
  fi

  echo "Missing ESP-IDF python for $wanted under $project_dir/.embuild/espressif/python_env" >&2
  return 1
}

find_esptool_py() {
  local project_dir="${1:?find_esptool_py requires project dir}"
  local wanted
  local python_env

  if [ -n "${IDF_PYTHON_ENV_PATH:-}" ] && [ -x "${IDF_PYTHON_ENV_PATH}/bin/esptool.py" ]; then
    printf '%s\n' "${IDF_PYTHON_ENV_PATH}/bin/esptool.py"
    return 0
  fi

  wanted="$(load_requested_idf_version "$project_dir")"
  python_env="$(find_requested_idf_python_env "$project_dir" "$wanted")"
  if [ -x "$python_env/bin/esptool.py" ]; then
    printf '%s\n' "$python_env/bin/esptool.py"
    return 0
  fi

  echo "Missing esptool.py for $wanted under $project_dir/.embuild/espressif/python_env" >&2
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
