#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
project_dir="${PROJECT_ROOT:-$PWD}"
gcc_wrapper="$(dirname "$(dirname "$(readlink -f "$(command -v gcc)")")")"
dynamic_linker="$(<"$gcc_wrapper/nix-support/dynamic-linker")"
alias_dir="$project_dir/.toolchain-bin"

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

patch_elf_interpreters "$project_dir/.rustup/toolchains/esp"
patch_elf_interpreters "$project_dir/.embuild/espressif/tools"
write_xtensa_dispatcher
write_xtensa_aliases
