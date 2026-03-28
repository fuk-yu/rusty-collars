#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_dir="${PROJECT_ROOT:-$(cd "$script_dir/.." && pwd)}"
gcc_wrapper="$(dirname "$(dirname "$(readlink -f "$(command -v gcc)")")")"
dynamic_linker="$(<"$gcc_wrapper/nix-support/dynamic-linker")"
alias_dir="$project_dir/.toolchain-bin"

path_prepend() {
  local path_entry="${1:?path_prepend requires a path}"

  case ":${PATH}:" in
    *":${path_entry}:"*) ;;
    *) export PATH="${path_entry}:${PATH}" ;;
  esac
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

  cat >"$alias_dir/xtensa-esp32-elf-dispatch" <<EOF
#!/usr/bin/env bash
set -euo pipefail

project_dir="$project_dir"
dynamic_linker="$dynamic_linker"
tool_name="\${0##*/}"
suffix="\${tool_name#xtensa-esp32-elf-}"
real_name="xtensa-esp-elf-\$suffix"
bindir=""

while IFS= read -r candidate; do
  bindir="\$candidate"
  break
done < <(find "\$project_dir/.embuild/espressif/tools/xtensa-esp-elf" -type d -path '*/xtensa-esp-elf/bin' | sort)

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
  while IFS= read -r bindir; do
    while IFS= read -r tool; do
      suffixes+=("${tool#xtensa-esp-elf-}")
    done < <(find "$bindir" -maxdepth 1 -type f -name 'xtensa-esp-elf-*' -printf '%f\n')
    break
  done < <(find "$project_dir/.embuild/espressif/tools/xtensa-esp-elf" -type d -path '*/xtensa-esp-elf/bin' | sort)

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

load_requested_idf_version() {
  grep 'esp_idf_version' "$project_dir/Cargo.toml" | sed 's/.*"\(.*\)"/\1/'
}

select_idf_path() {
  local requested_version="${1:-}"
  local candidate current_version

  for candidate in "$project_dir"/.embuild/espressif/esp-idf/v5.*; do
    [ -d "$candidate/.git" ] || continue
    current_version="$(
      git -C "$candidate" describe --tags --exact-match 2>/dev/null \
        || git -C "$candidate" describe --tags 2>/dev/null \
        || true
    )"
    if [ -n "$requested_version" ] && [ "$current_version" = "$requested_version" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  for candidate in "$project_dir"/.embuild/espressif/esp-idf/v5.*; do
    [ -d "$candidate" ] || continue
    printf '%s\n' "$candidate"
    return 0
  done

  return 0
}

select_python_env() {
  local candidate

  for candidate in "$project_dir"/.embuild/espressif/python_env/idf5.*; do
    [ -d "$candidate" ] || continue
    printf '%s\n' "$candidate"
    return 0
  done

  return 0
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

patch_elf_interpreters "$project_dir/.rustup/toolchains/esp"
patch_elf_interpreters "$project_dir/.embuild/espressif/tools"
write_xtensa_dispatcher
write_xtensa_aliases
path_prepend "$alias_dir"

while IFS= read -r clang_lib; do
  export LIBCLANG_PATH="$clang_lib"
  break
done < <(find "$project_dir/.rustup/toolchains/esp" -type d -path '*/esp-clang/lib' | sort)

requested_idf_version="$(load_requested_idf_version)"
idf_path="$(select_idf_path "$requested_idf_version")"
if [ -n "$idf_path" ]; then
  export IDF_PATH="$idf_path"
  export IDF_TOOLS_PATH="$project_dir/.embuild/espressif"
  path_prepend "$IDF_PATH/tools"
fi

python_env_path="$(select_python_env)"
if [ -n "$python_env_path" ]; then
  rewrite_python_env_shebangs "$python_env_path"
  export IDF_PYTHON_ENV_PATH="$python_env_path"
  path_prepend "$IDF_PYTHON_ENV_PATH/bin"
fi

while IFS= read -r tool_dir; do
  path_prepend "$tool_dir"
done < <(find "$project_dir/.embuild/espressif/tools/xtensa-esp-elf" -type d -path '*/xtensa-esp-elf/bin' | sort -r)

while IFS= read -r tool_dir; do
  path_prepend "$tool_dir"
done < <(find "$project_dir/.embuild/espressif/tools/riscv32-esp-elf" -type d -path '*/riscv32-esp-elf/bin' | sort -r)

esp_rom_elf_dir="$(select_esp_rom_elf_dir)"
if [ -n "$esp_rom_elf_dir" ]; then
  export ESP_ROM_ELF_DIR="$esp_rom_elf_dir"
fi
