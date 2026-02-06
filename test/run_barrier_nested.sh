#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/barrier_nested.toml"
ELF_PATH="test/performance-model-tests/binaries/barrier_nested.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"
jq '{scheduler_cycles: .total.scheduler.cycles, active_warps_sum: .total.scheduler.active_warps_sum, eligible_warps_sum: .total.scheduler.eligible_warps_sum, issued_warps_sum: .total.scheduler.issued_warps_sum, barrier_arrivals: .total.barrier_summary.arrivals, barrier_release_events: .total.barrier_summary.release_events, barrier_warps_released: .total.barrier_summary.warps_released, barrier_queue_rejects: .total.barrier_summary.queue_rejects, barrier_total_wait_cycles: .total.barrier_summary.total_scheduled_wait_cycles, barrier_max_wait_cycles: .total.barrier_summary.max_scheduled_wait_cycles, gmem_queue_full: .total.stall_summary.gmem_queue_full, gmem_busy: .total.stall_summary.gmem_busy, smem_queue_full: .total.stall_summary.smem_queue_full, smem_busy: .total.stall_summary.smem_busy}' \
  "${RUN_DIR}/summary.json"
