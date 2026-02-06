#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/lsu_shared_stq_fill.toml"
ELF_PATH="test/performance-model-tests/binaries/lsu_shared_stq_fill.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"

jq '{lsu_issued: .total.lsu_stats.issued, lsu_completed: .total.lsu_stats.completed, lsu_queue_full: .total.lsu_stats.queue_full_rejects, lsu_busy: .total.lsu_stats.busy_rejects, shared_stq_issued: .total.lsu_stats.shared_stq_issued, global_ldq_queue_full_rejects: .total.lsu_stats.global_ldq_queue_full_rejects, global_stq_queue_full_rejects: .total.lsu_stats.global_stq_queue_full_rejects, shared_ldq_queue_full_rejects: .total.lsu_stats.shared_ldq_queue_full_rejects, shared_stq_queue_full_rejects: .total.lsu_stats.shared_stq_queue_full_rejects, smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, gmem_issued: .total.gmem_stats.issued, l0_accesses: .total.gmem_hits.l0_accesses}' "${RUN_DIR}/summary.json"

lsu_issued="$(jq -r '.total.lsu_stats.issued' "${RUN_DIR}/summary.json")"
lsu_queue_full="$(jq -r '.total.lsu_stats.queue_full_rejects' "${RUN_DIR}/summary.json")"
shared_stq_issued="$(jq -r '.total.lsu_stats.shared_stq_issued' "${RUN_DIR}/summary.json")"
global_ldq_qf="$(jq -r '.total.lsu_stats.global_ldq_queue_full_rejects' "${RUN_DIR}/summary.json")"
global_stq_qf="$(jq -r '.total.lsu_stats.global_stq_queue_full_rejects' "${RUN_DIR}/summary.json")"
shared_ldq_qf="$(jq -r '.total.lsu_stats.shared_ldq_queue_full_rejects' "${RUN_DIR}/summary.json")"
shared_stq_qf="$(jq -r '.total.lsu_stats.shared_stq_queue_full_rejects' "${RUN_DIR}/summary.json")"
smem_issued="$(jq -r '.total.smem_stats.issued' "${RUN_DIR}/summary.json")"
gmem_issued="$(jq -r '.total.gmem_stats.issued' "${RUN_DIR}/summary.json")"

if [[ "$lsu_issued" -gt 0 && "$shared_stq_issued" -gt 0 && "$smem_issued" -gt 0 && "$shared_stq_qf" -gt 0 && "$global_ldq_qf" -eq 0 && "$global_stq_qf" -eq 0 && "$shared_ldq_qf" -eq 0 ]]; then
  echo "LSU shared_stq_fill check: PASS"
else
  echo "LSU shared_stq_fill check: FAIL"
  echo "  lsu_issued=${lsu_issued}"
  echo "  shared_stq_issued=${shared_stq_issued}"
  echo "  smem_issued=${smem_issued}"
  echo "  gmem_issued=${gmem_issued}"
  echo "  lsu_queue_full=${lsu_queue_full}"
  echo "  global_ldq_queue_full_rejects=${global_ldq_qf}"
  echo "  global_stq_queue_full_rejects=${global_stq_qf}"
  echo "  shared_ldq_queue_full_rejects=${shared_ldq_qf}"
  echo "  shared_stq_queue_full_rejects=${shared_stq_qf}"
  exit 1
fi
