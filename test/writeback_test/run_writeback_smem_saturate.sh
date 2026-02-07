#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/writeback_smem_saturate.toml"
ELF_PATH="test/performance-model-tests/binaries/writeback_smem_saturate.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | sed -n '1p')"
echo "Latest run: ${RUN_DIR}"

jq '{writeback_issued: .total.writeback_stats.issued, writeback_completed: .total.writeback_stats.completed, writeback_queue_full: .total.writeback_stats.queue_full_rejects, writeback_busy: .total.writeback_stats.busy_rejects, smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, gmem_completed: .total.gmem_stats.completed}' "${RUN_DIR}/summary.json"

wb_issued="$(jq -r '.total.writeback_stats.issued' "${RUN_DIR}/summary.json")"
wb_completed="$(jq -r '.total.writeback_stats.completed' "${RUN_DIR}/summary.json")"
wb_qf="$(jq -r '.total.writeback_stats.queue_full_rejects' "${RUN_DIR}/summary.json")"
smem_completed="$(jq -r '.total.smem_stats.completed' "${RUN_DIR}/summary.json")"
gmem_completed="$(jq -r '.total.gmem_stats.completed' "${RUN_DIR}/summary.json")"

if [[ "$wb_issued" -gt 0 && "$wb_completed" -gt 0 && "$wb_qf" -gt 0 && "$smem_completed" -gt 0 && "$wb_completed" -ge "$smem_completed" && "$wb_completed" -ge "$gmem_completed" ]]; then
  echo "Writeback smem_saturate check: PASS"
else
  echo "Writeback smem_saturate check: FAIL"
  echo "  writeback_issued=${wb_issued}"
  echo "  writeback_completed=${wb_completed}"
  echo "  writeback_queue_full=${wb_qf}"
  echo "  smem_completed=${smem_completed}"
  echo "  gmem_completed=${gmem_completed}"
  exit 1
fi
