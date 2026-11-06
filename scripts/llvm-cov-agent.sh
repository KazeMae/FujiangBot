#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"
if ! rustup component list --installed | grep -q '^llvm-tools'; then
  rustup component add llvm-tools-preview
fi
if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  cargo install cargo-llvm-cov --locked
fi
exec cargo llvm-cov -p fujiang-plugin-agent --summary-only "$@"
