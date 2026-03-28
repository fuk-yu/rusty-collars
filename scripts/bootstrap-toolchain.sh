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
  mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"

  if cargo +esp --version >/dev/null 2>&1; then
    echo "Repo-local ESP toolchain already installed."
    "$PROJECT_ROOT/scripts/prepare-toolchain-env.sh"
    exit 0
  fi

  espup install --std --targets esp32,esp32c6,esp32p4 --name esp --export-file "$ESPUP_EXPORT_FILE"
  . "$ESPUP_EXPORT_FILE"
  "$PROJECT_ROOT/scripts/prepare-toolchain-env.sh"
  cargo +esp --version
'
