#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

run_in_env() {
  if [[ -n "${DIRENV_DIR:-}" ]]; then
    "$@"
  else
    direnv exec "$project_dir" "$@"
  fi
}

run_in_env bash -lc '
  set -euo pipefail
  export CARGO_HOME="$PROJECT_ROOT/.cargo-home"
  export RUSTUP_HOME="$PROJECT_ROOT/.rustup"
  export ESPUP_EXPORT_FILE="$PROJECT_ROOT/export-esp.sh"
  unset IDF_PATH IDF_PYTHON_ENV_PATH IDF_TOOLS_PATH ESP_ROM_ELF_DIR
  mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"
  . "$PROJECT_ROOT/scripts/esp-idf-env.sh"

  if cargo +esp --version >/dev/null 2>&1; then
    echo "Repo-local ESP toolchain already installed."
  else
    espup install --std --targets esp32,esp32c6,esp32p4 --name esp --export-file "$ESPUP_EXPORT_FILE"
    . "$ESPUP_EXPORT_FILE"
  fi

  # Set esp as default so that pip can find cargo when building Python
  # packages with Rust extensions (e.g. pydantic-core).
  rustup default esp

  requested_idf_version="$(load_requested_idf_version "$PROJECT_ROOT")"
  idf_path="$(requested_idf_checkout_path "$PROJECT_ROOT" "$requested_idf_version")"
  mkdir -p "$(dirname "$idf_path")"
  if [ ! -d "$idf_path/.git" ]; then
    git clone --branch "$requested_idf_version" --recursive https://github.com/espressif/esp-idf.git "$idf_path"
  else
    current_idf_version="$(git -C "$idf_path" describe --tags --exact-match 2>/dev/null || true)"
    if [ "$current_idf_version" != "$requested_idf_version" ]; then
      git -C "$idf_path" fetch --tags --quiet
      git -C "$idf_path" checkout "$requested_idf_version" --quiet
      git -C "$idf_path" submodule update --init --recursive --quiet
    fi
  fi

  export IDF_TOOLS_PATH="$PROJECT_ROOT/.embuild/espressif"
  python3 "$idf_path/tools/idf_tools.py" install-python-env
  python3 "$idf_path/tools/idf_tools.py" download --targets esp32,esp32c6,esp32p4 required
  # Drop stale CMake/compiler caches after an ESP-IDF or GCC toolchain upgrade.
  cargo +esp clean 2>/dev/null || true
  cargo +esp --version
'
