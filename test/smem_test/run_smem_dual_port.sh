#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

ELF_PATH="test/performance-model-tests/binaries/smem_dual_port.elf"
LANES=16
BANKS=8

run_case() {
  local label="$1"
  local config_path="$2"

  cargo run --release -- "$config_path" --binary-path "$ELF_PATH"

  local run_dir
  run_dir="$(ls -td performance_logs/run_* | head -n 1)"
  echo "${label} run: ${run_dir}"

  jq '{scheduler_cycles: .total.scheduler.cycles, smem_issued: .total.smem_stats.issued, smem_read_issued: .total.smem_stats.read_issued, smem_write_issued: .total.smem_stats.write_issued, smem_completed: .total.smem_stats.completed, smem_read_completed: .total.smem_stats.read_completed, smem_write_completed: .total.smem_stats.write_completed, smem_queue_full: .total.smem_stats.queue_full_rejects, smem_busy: .total.smem_stats.busy_rejects, smem_conflict_instructions: .total.smem_conflicts.instructions, smem_conflict_active_lanes: .total.smem_conflicts.active_lanes, smem_conflict_lanes: .total.smem_conflicts.conflict_lanes, smem_unique_banks: .total.smem_conflicts.unique_banks, smem_unique_subbanks: .total.smem_conflicts.unique_subbanks}' "${run_dir}/summary.json"

  local instructions active_lanes conflict_lanes unique_banks
  instructions="$(jq -r '.total.smem_conflicts.instructions' "${run_dir}/summary.json")"
  active_lanes="$(jq -r '.total.smem_conflicts.active_lanes' "${run_dir}/summary.json")"
  conflict_lanes="$(jq -r '.total.smem_conflicts.conflict_lanes' "${run_dir}/summary.json")"
  unique_banks="$(jq -r '.total.smem_conflicts.unique_banks' "${run_dir}/summary.json")"

  local expected_active expected_conflicts expected_unique_banks
  expected_active=$((instructions * LANES))
  expected_conflicts=$((instructions * (LANES - BANKS)))
  expected_unique_banks=$((instructions * BANKS))

  if [[ "$instructions" -le 0 || "$active_lanes" -ne "$expected_active" || "$conflict_lanes" -ne "$expected_conflicts" || "$unique_banks" -ne "$expected_unique_banks" ]]; then
    echo "${label} conflict-model check: FAIL"
    echo "  instructions=${instructions}"
    echo "  active_lanes=${active_lanes} expected=${expected_active}"
    echo "  conflict_lanes=${conflict_lanes} expected=${expected_conflicts}"
    echo "  unique_banks=${unique_banks} expected=${expected_unique_banks}"
    exit 1
  fi

  local cycles queue_full busy read_completed write_completed
  cycles="$(jq -r '.total.scheduler.cycles' "${run_dir}/summary.json")"
  queue_full="$(jq -r '.total.smem_stats.queue_full_rejects' "${run_dir}/summary.json")"
  busy="$(jq -r '.total.smem_stats.busy_rejects' "${run_dir}/summary.json")"
  read_completed="$(jq -r '.total.smem_stats.read_completed' "${run_dir}/summary.json")"
  write_completed="$(jq -r '.total.smem_stats.write_completed' "${run_dir}/summary.json")"
  local bank_total
  bank_total="$(jq -r '.total.smem_util.bank_total' "${run_dir}/summary.json")"

  echo "${label} conflict-model check: PASS"
  echo "${label} cycles=${cycles} queue_full=${queue_full} busy=${busy} bank_total=${bank_total} read_completed=${read_completed} write_completed=${write_completed}"

  case "$label" in
    single_port)
      SINGLE_PORT_CYCLES="$cycles"
      SINGLE_PORT_QUEUE_FULL="$queue_full"
      SINGLE_PORT_BUSY="$busy"
      SINGLE_PORT_BANK_TOTAL="$bank_total"
      ;;
    dual_port)
      DUAL_PORT_CYCLES="$cycles"
      DUAL_PORT_QUEUE_FULL="$queue_full"
      DUAL_PORT_BUSY="$busy"
      DUAL_PORT_BANK_TOTAL="$bank_total"
      ;;
  esac
}

run_case "single_port" "test/performance-model-tests/configs/smem_dual_port_off.toml"
run_case "dual_port" "test/performance-model-tests/configs/smem_dual_port_on.toml"

if [[ "$DUAL_PORT_BANK_TOTAL" -eq $((SINGLE_PORT_BANK_TOTAL * 2)) ]]; then
  echo "SMEM dual-port topology check: PASS (bank_total ${SINGLE_PORT_BANK_TOTAL} -> ${DUAL_PORT_BANK_TOTAL})"
else
  echo "SMEM dual-port topology check: FAIL"
  echo "  single_port bank_total=${SINGLE_PORT_BANK_TOTAL}"
  echo "  dual_port bank_total=${DUAL_PORT_BANK_TOTAL}"
  exit 1
fi

if [[ "$DUAL_PORT_CYCLES" -lt "$SINGLE_PORT_CYCLES" ]]; then
  echo "SMEM dual-port throughput observation: dual_port cycles ${DUAL_PORT_CYCLES} < single_port cycles ${SINGLE_PORT_CYCLES}"
else
  echo "SMEM dual-port throughput observation: no cycle reduction (${DUAL_PORT_CYCLES} vs ${SINGLE_PORT_CYCLES}) for this traffic shape"
fi
