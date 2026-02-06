#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/smem_full_conflict.toml"
ELF_PATH="test/performance-model-tests/binaries/smem_full_conflict.elf"
LANES=16

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"

jq '{scheduler_cycles: .total.scheduler.cycles, smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, smem_queue_full: .total.smem_stats.queue_full_rejects, smem_busy: .total.smem_stats.busy_rejects, smem_conflict_instructions: .total.smem_conflicts.instructions, smem_conflict_active_lanes: .total.smem_conflicts.active_lanes, smem_conflict_lanes: .total.smem_conflicts.conflict_lanes, smem_unique_banks: .total.smem_conflicts.unique_banks, smem_unique_subbanks: .total.smem_conflicts.unique_subbanks}' "${RUN_DIR}/summary.json"

instructions="$(jq -r '.total.smem_conflicts.instructions' "${RUN_DIR}/summary.json")"
active_lanes="$(jq -r '.total.smem_conflicts.active_lanes' "${RUN_DIR}/summary.json")"
conflict_lanes="$(jq -r '.total.smem_conflicts.conflict_lanes' "${RUN_DIR}/summary.json")"
unique_banks="$(jq -r '.total.smem_conflicts.unique_banks' "${RUN_DIR}/summary.json")"

expected_active=$((instructions * LANES))
expected_conflicts=$((instructions * (LANES - 1)))
expected_unique_banks=$instructions

if [[ "$instructions" -gt 0 && "$active_lanes" -eq "$expected_active" && "$conflict_lanes" -eq "$expected_conflicts" && "$unique_banks" -eq "$expected_unique_banks" ]]; then
  echo "SMEM full-conflict check: PASS"
else
  echo "SMEM full-conflict check: FAIL"
  echo "  instructions=${instructions}"
  echo "  active_lanes=${active_lanes} expected=${expected_active}"
  echo "  conflict_lanes=${conflict_lanes} expected=${expected_conflicts}"
  echo "  unique_banks=${unique_banks} expected=${expected_unique_banks}"
  exit 1
fi
