#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
project_dir="$PWD"

source "$project_dir/scripts/target-info.sh"

TARGET_NAME="${1:?Usage: $0 <$SUPPORTED_TARGETS>}"
activate_target "$project_dir" "$TARGET_NAME"

echo "Target: $TARGET_NAME ($TARGET_TRIPLE)"
echo "Config updated. Run 'cargo clean' then build."
