#!/usr/bin/env bash
set -euo pipefail

load_requested_idf_version() {
  local project_dir="${1:?load_requested_idf_version requires project dir}"

  grep 'esp_idf_version' "$project_dir/Cargo.toml" | sed 's/.*"\(.*\)"/\1/'
}

requested_idf_series() {
  local requested_version="${1:?requested_idf_series requires ESP-IDF version}"

  printf '%s\n' "$requested_version" | sed -E 's/^v([0-9]+\.[0-9]+).*/\1/'
}

requested_idf_checkout_path() {
  local project_dir="${1:?requested_idf_checkout_path requires project dir}"
  local requested_version="${2:?requested_idf_checkout_path requires ESP-IDF version}"

  printf '%s/.embuild/espressif/esp-idf/%s\n' "$project_dir" "$requested_version"
}

current_tool_archive_suffix() {
  local system
  local machine

  system="$(uname -s)"
  machine="$(uname -m)"

  case "${system}:${machine}" in
    Linux:x86_64) printf '%s\n' "x86_64-linux-gnu" ;;
    Linux:aarch64) printf '%s\n' "aarch64-linux-gnu" ;;
    Darwin:x86_64) printf '%s\n' "x86_64-apple-darwin" ;;
    Darwin:arm64) printf '%s\n' "aarch64-apple-darwin" ;;
    *)
      echo "Unsupported host platform: ${system}/${machine}" >&2
      return 1
      ;;
  esac
}

find_requested_idf_python_env() {
  local project_dir="${1:?find_requested_idf_python_env requires project dir}"
  local requested_version="${2:?find_requested_idf_python_env requires ESP-IDF version}"
  local series
  local candidate

  series="$(requested_idf_series "$requested_version")"
  while IFS= read -r candidate; do
    printf '%s\n' "$candidate"
    return 0
  done < <(find "$project_dir/.embuild/espressif/python_env" -maxdepth 1 -mindepth 1 -type d -name "idf${series}_py*_env" | sort -r)

  return 0
}

find_toolchain_bin_dir() {
  local project_dir="${1:?find_toolchain_bin_dir requires project dir}"
  local tool_name="${2:?find_toolchain_bin_dir requires tool name}"
  local tools_root="$project_dir/.embuild/espressif/tools/$tool_name"
  local candidate
  local wanted_version=""

  [ -d "$tools_root" ] || return 0

  # Try to match the toolchain version required by the active ESP-IDF.
  # tools.json lists exact versions; grep for the tool name's version string.
  if [ -n "${IDF_PATH:-}" ] && [ -f "$IDF_PATH/tools/tools.json" ]; then
    wanted_version=$(python3 -c "
import json, sys
with open('$IDF_PATH/tools/tools.json') as f:
    data = json.load(f)
for tool in data.get('tools', []):
    if tool.get('name') == '$tool_name':
        for v in tool.get('versions', []):
            if v.get('status') == 'recommended':
                print(v['name'])
                sys.exit(0)
" 2>/dev/null || true)
  fi

  if [ -n "$wanted_version" ] && [ -d "$tools_root/$wanted_version/$tool_name/bin" ]; then
    printf '%s\n' "$tools_root/$wanted_version/$tool_name/bin"
    return 0
  fi

  # Fallback: pick the newest installed version.
  while IFS= read -r candidate; do
    printf '%s\n' "$candidate"
  done < <(
    find "$tools_root" -maxdepth 3 -type d -path "*/$tool_name/bin" | sort | tail -n 1
  )
}

materialize_downloaded_toolchains() {
  local project_dir="${1:?materialize_downloaded_toolchains requires project dir}"
  local dist_dir="$project_dir/.embuild/espressif/dist"
  local tools_dir="$project_dir/.embuild/espressif/tools"
  local suffix
  local tool
  local archive
  local base
  local ext
  local version
  local version_dir

  [ -d "$dist_dir" ] || return 0

  suffix="$(current_tool_archive_suffix)"
  for tool in xtensa-esp-elf riscv32-esp-elf; do
    while IFS= read -r archive; do
      base="${archive##*/}"
      case "$base" in
        *.tar.xz) ext=".tar.xz" ;;
        *.tar.gz) ext=".tar.gz" ;;
        *) continue ;;
      esac

      version="${base#${tool}-}"
      version="${version%-${suffix}${ext}}"
      case "$version" in
        esp-*) version_dir="$tools_dir/$tool/$version" ;;
        *) version_dir="$tools_dir/$tool/esp-$version" ;;
      esac

      [ -d "$version_dir/$tool/bin" ] && continue

      rm -rf "$version_dir"
      mkdir -p "$version_dir"
      case "$ext" in
        .tar.xz) tar -xJf "$archive" -C "$version_dir" ;;
        .tar.gz) tar -xzf "$archive" -C "$version_dir" ;;
      esac
    done < <(find "$dist_dir" -maxdepth 1 -type f \( -name "${tool}-*-${suffix}.tar.xz" -o -name "${tool}-*-${suffix}.tar.gz" \) | sort)
  done
}
