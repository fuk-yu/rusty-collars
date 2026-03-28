#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"
parse_target_arg "$@"

if [[ -n "${DIRENV_DIR:-}" ]]; then
  espflash monitor $ESPFLASH_MONITOR_ARGS
else
  direnv exec "$project_dir" espflash monitor $ESPFLASH_MONITOR_ARGS
fi
