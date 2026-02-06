#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

ELF_PATH="test/performance-model-tests/binaries/op_fetch_queue_fill.elf"
BASELINE_CFG="test/performance-model-tests/configs/op_fetch_queue_fill_baseline.toml"
QUEUE_FILL_CFG="test/performance-model-tests/configs/op_fetch_queue_fill.toml"

run_case() {
  local label="$1"
  local cfg="$2"
  local __cycles_var="$3"

  cargo run --release -- "$cfg" --binary-path "$ELF_PATH"
  local run_dir
  run_dir="$(ls -td performance_logs/run_* | head -n 1)"
  local cycles
  cycles="$(jq -r '.total.scheduler.cycles' "${run_dir}/summary.json")"

  echo "Latest run (${label}): ${run_dir}"
  jq '{scheduler_cycles: .total.scheduler.cycles, issued_warps: .total.scheduler.issued_warps_sum, gmem_issued: .total.gmem_stats.issued, l0_accesses: .total.gmem_hits.l0_accesses, l1_accesses: .total.gmem_hits.l1_accesses, l2_accesses: .total.gmem_hits.l2_accesses, lsu_issued: .total.lsu_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects, lsu_busy: .total.lsu_stats.busy_rejects}' "${run_dir}/summary.json"
  printf -v "$__cycles_var" '%s' "$cycles"
}

baseline_cycles=0
queue_fill_cycles=0
run_case "baseline (operand_fetch disabled)" "$BASELINE_CFG" baseline_cycles
run_case "queue_fill (operand_fetch enabled)" "$QUEUE_FILL_CFG" queue_fill_cycles

echo "Cycle delta (enabled - baseline): $((queue_fill_cycles - baseline_cycles))"
awk -v b="$baseline_cycles" -v q="$queue_fill_cycles" 'BEGIN { if (b == 0) { print "Cycle ratio (enabled/baseline): inf" } else { printf "Cycle ratio (enabled/baseline): %.3f\n", q / b } }'
