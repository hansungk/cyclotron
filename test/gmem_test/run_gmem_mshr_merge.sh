#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1
export CYCLOTRON_GRAPH_LOG=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/gmem_mshr_merge.toml"
ELF_PATH="test/performance-model-tests/binaries/gmem_mshr_merge.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | sed -n '1p')"
echo "Latest run: ${RUN_DIR}"
jq '{l0_accesses: .total.gmem_hits.l0_accesses, l0_hits: .total.gmem_hits.l0_hits, l1_accesses: .total.gmem_hits.l1_accesses, l1_hits: .total.gmem_hits.l1_hits, l2_accesses: .total.gmem_hits.l2_accesses, l2_hits: .total.gmem_hits.l2_hits, issued: .total.gmem_stats.issued, queue_full_rejects: .total.gmem_stats.queue_full_rejects, busy_rejects: .total.gmem_stats.busy_rejects}' \
  "${RUN_DIR}/summary.json"
