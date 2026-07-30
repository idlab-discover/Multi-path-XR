#!/usr/bin/env bash
set -euo pipefail

# Determine repository root based on script location.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required but not found. Please install Rust toolchain." >&2
  exit 1
fi

(
  cd "${REPO_ROOT}"
  cargo run --package virtual-wall --bin vw -- "$@"
)
