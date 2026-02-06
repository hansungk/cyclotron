#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/writeback_gmem_saturate.toml"
ELF_PATH="test/performance-model-tests/binaries/writeback_gmem_saturate.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"

jq '{writeback_issued: .total.writeback_stats.issued, writeback_completed: .total.writeback_stats.completed, writeback_queue_full: .total.writeback_stats.queue_full_rejects, writeback_busy: .total.writeback_stats.busy_rejects, gmem_issued: .total.gmem_stats.issued, gmem_completed: .total.gmem_stats.completed, lsu_issued: .total.lsu_stats.issued}' "${RUN_DIR}/summary.json"

wb_issued="$(jq -r '.total.writeback_stats.issued' "${RUN_DIR}/summary.json")"
wb_completed="$(jq -r '.total.writeback_stats.completed' "${RUN_DIR}/summary.json")"
wb_qf="$(jq -r '.total.writeback_stats.queue_full_rejects' "${RUN_DIR}/summary.json")"
gmem_completed="$(jq -r '.total.gmem_stats.completed' "${RUN_DIR}/summary.json")"

if [[ "$wb_issued" -gt 0 && "$wb_completed" -gt 0 && "$wb_qf" -gt 0 && "$wb_completed" -eq "$gmem_completed" ]]; then
  echo "Writeback gmem_saturate check: PASS"
else
  echo "Writeback gmem_saturate check: FAIL"
  echo "  writeback_issued=${wb_issued}"
  echo "  writeback_completed=${wb_completed}"
  echo "  writeback_queue_full=${wb_qf}"
  echo "  gmem_completed=${gmem_completed}"
  exit 1
fi
