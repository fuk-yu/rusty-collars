#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_dir="${PROJECT_ROOT:-$(cd "$script_dir/.." && pwd)}"
. "$script_dir/esp-idf-env.sh"
gcc_wrapper="$(dirname "$(dirname "$(readlink -f "$(command -v gcc)")")")"
dynamic_linker="$(<"$gcc_wrapper/nix-support/dynamic-linker")"
alias_dir="$project_dir/.toolchain-bin"

die() {
  echo "$*" >&2
  return 1 2>/dev/null || exit 1
}

path_prepend() {
  local path_entry="${1:?path_prepend requires a path}"

  case ":${PATH}:" in
    *":${path_entry}:"*) ;;
    *) export PATH="${path_entry}:${PATH}" ;;
  esac
}

sanitize_embuild_path_entries() {
  local sanitized_path=""
  local path_entry
  local old_ifs="$IFS"

  IFS=':'
  for path_entry in $PATH; do
    case "$path_entry" in
      "$alias_dir"|\
      "$project_dir/.embuild/espressif/tools/"*|\
      "$project_dir/.embuild/espressif/python_env/"*|\
      "$project_dir/.embuild/espressif/esp-idf/"*/tools)
        continue
        ;;
    esac

    if [ -z "$sanitized_path" ]; then
      sanitized_path="$path_entry"
    else
      sanitized_path="${sanitized_path}:$path_entry"
    fi
  done
  IFS="$old_ifs"

  export PATH="$sanitized_path"
}

patch_elf_interpreters() {
  local root="$1"
  [ -d "$root" ] || return 0

  while IFS= read -r -d '' path; do
    if interpreter="$(patchelf --print-interpreter "$path" 2>/dev/null)"; then
      if [ "$interpreter" != "$dynamic_linker" ]; then
        patchelf --set-interpreter "$dynamic_linker" "$path"
      fi
    fi
  done < <(find "$root" -type f -perm -u+x -print0)
}

write_xtensa_dispatcher() {
  mkdir -p "$alias_dir"

  local bindir
  bindir="$(find_toolchain_bin_dir "$project_dir" xtensa-esp-elf)"

  cat >"$alias_dir/xtensa-esp32-elf-dispatch" <<EOF
#!/usr/bin/env bash
set -euo pipefail

project_dir="$project_dir"
dynamic_linker="$dynamic_linker"
bindir="$bindir"
tool_name="\${0##*/}"
suffix="\${tool_name#xtensa-esp32-elf-}"
real_name="xtensa-esp-elf-\$suffix"

[ -n "\$bindir" ] || {
  echo "Missing ESP-IDF xtensa toolchain under \$project_dir/.embuild" >&2
  exit 1
}

target="\$bindir/\$real_name"
[ -x "\$target" ] || {
  echo "Missing tool: \$target" >&2
  exit 1
}

if interpreter="\$(patchelf --print-interpreter "\$target" 2>/dev/null)"; then
  if [ "\$interpreter" != "\$dynamic_linker" ]; then
    patchelf --set-interpreter "\$dynamic_linker" "\$target"
  fi
fi

exec "\$target" "\$@"
EOF

  chmod +x "$alias_dir/xtensa-esp32-elf-dispatch"
}

write_xtensa_aliases() {
  local -a suffixes=(
    addr2line
    ar
    as
    c++
    c++filt
    cc
    cpp
    elfedit
    g++
    gcc
    gcc-ar
    gcc-nm
    gcc-ranlib
    gcov
    gcov-dump
    gcov-tool
    gprof
    ld
    ld.bfd
    nm
    objcopy
    objdump
    ranlib
    readelf
    size
    strings
    strip
  )

  local bindir
  bindir="$(find_toolchain_bin_dir "$project_dir" xtensa-esp-elf)"
  if [ -n "$bindir" ]; then
    while IFS= read -r tool; do
      suffixes+=("${tool#xtensa-esp-elf-}")
    done < <(find "$bindir" -maxdepth 1 -type f -name 'xtensa-esp-elf-*' -printf '%f\n')
  fi

  printf '%s\n' "${suffixes[@]}" | sort -u | while IFS= read -r suffix; do
    ln -snf xtensa-esp32-elf-dispatch "$alias_dir/xtensa-esp32-elf-$suffix"
  done
}

rewrite_python_env_shebangs() {
  local python_env="${1:?rewrite_python_env_shebangs requires a python env dir}"
  local python_bin="$python_env/bin/python"
  local wrapper first_line interpreter tmp

  [ -x "$python_bin" ] || return 0

  while IFS= read -r -d '' wrapper; do
    IFS= read -r first_line < "$wrapper" || continue
    case "$first_line" in
      '#!'*/bin/python|'#!'*/bin/python3)
        interpreter="${first_line#\#!}"
        if [ "$interpreter" = "$python_bin" ] || [ -x "$interpreter" ]; then
          continue
        fi
        tmp="$(mktemp)"
        {
          printf '#!%s\n' "$python_bin"
          tail -n +2 "$wrapper"
        } > "$tmp"
        chmod --reference="$wrapper" "$tmp"
        mv "$tmp" "$wrapper"
        ;;
    esac
  done < <(find "$python_env/bin" -maxdepth 1 -type f ! -name 'python' ! -name 'python*' -print0)
}

select_esp_rom_elf_dir() {
  local candidate

  for candidate in "$project_dir"/.embuild/espressif/tools/esp-rom-elfs/*; do
    [ -d "$candidate" ] || continue
    printf '%s\n' "$candidate"
    return 0
  done

  return 0
}

materialize_downloaded_toolchains "$project_dir"
patch_elf_interpreters "$project_dir/.rustup/toolchains/esp"
patch_elf_interpreters "$project_dir/.embuild/espressif/tools"
write_xtensa_dispatcher
write_xtensa_aliases
sanitize_embuild_path_entries
path_prepend "$alias_dir"

while IFS= read -r clang_lib; do
  export LIBCLANG_PATH="$clang_lib"
  break
done < <(find "$project_dir/.rustup/toolchains/esp" -type d -path '*/esp-clang/lib' | sort)

requested_idf_version="$(load_requested_idf_version "$project_dir")"
[ -n "$requested_idf_version" ] || die "Missing esp_idf_version in $project_dir/Cargo.toml"

idf_path="$(requested_idf_checkout_path "$project_dir" "$requested_idf_version")"
if [ ! -d "$idf_path/.git" ]; then
  echo "ESP-IDF $requested_idf_version not found, running bootstrap-toolchain.sh..."
  "$project_dir/scripts/bootstrap-toolchain.sh"
fi
[ -d "$idf_path/.git" ] || die "bootstrap-toolchain.sh failed to install ESP-IDF $requested_idf_version"

export IDF_PATH="$idf_path"
export IDF_TOOLS_PATH="$project_dir/.embuild/espressif"
path_prepend "$IDF_PATH/tools"

python_env_path="$(find_requested_idf_python_env "$project_dir" "$requested_idf_version")"
if [ -z "$python_env_path" ]; then
  echo "ESP-IDF Python environment for $requested_idf_version not found, running bootstrap-toolchain.sh..."
  "$project_dir/scripts/bootstrap-toolchain.sh"
  python_env_path="$(find_requested_idf_python_env "$project_dir" "$requested_idf_version")"
fi
[ -n "$python_env_path" ] || die "bootstrap-toolchain.sh failed to install ESP-IDF Python environment for $requested_idf_version"

rewrite_python_env_shebangs "$python_env_path"
export IDF_PYTHON_ENV_PATH="$python_env_path"
path_prepend "$IDF_PYTHON_ENV_PATH/bin"

xtensa_tool_dir="$(find_toolchain_bin_dir "$project_dir" xtensa-esp-elf)"
if [ -n "$xtensa_tool_dir" ]; then
  path_prepend "$xtensa_tool_dir"
fi

riscv_tool_dir="$(find_toolchain_bin_dir "$project_dir" riscv32-esp-elf)"
if [ -n "$riscv_tool_dir" ]; then
  path_prepend "$riscv_tool_dir"
fi

esp_rom_elf_dir="$(select_esp_rom_elf_dir)"
if [ -n "$esp_rom_elf_dir" ]; then
  export ESP_ROM_ELF_DIR="$esp_rom_elf_dir"
fi
