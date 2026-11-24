#!/usr/bin/env bash
# Shared verification entrypoint for local dev and CI (local == CI).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== [1/3] Frontend type check: npx tsc --noEmit ==="
npx tsc --noEmit

echo "=== [2/3] Rust build: cargo build ==="
(cd src-tauri && cargo build)

echo "=== [3/3] Rust tests: cargo test ==="
(cd src-tauri && cargo test)

echo "=== All checks passed ==="
