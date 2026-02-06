#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1
export CYCLOTRON_GRAPH_LOG=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/gmem_bank_select.toml"
ELF_PATH="test/performance-model-tests/binaries/gmem_bank_select.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"
