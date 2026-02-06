#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

CONFIG_PATH="test/performance-model-tests/configs/barrier_skewed.toml"
ELF_PATH="test/performance-model-tests/binaries/barrier_skewed.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"

RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"
jq '{scheduler_cycles: .total.scheduler.cycles, issued_warps_sum: .total.scheduler.issued_warps_sum, barrier_arrivals: .total.barrier_summary.arrivals, barrier_release_events: .total.barrier_summary.release_events, barrier_warps_released: .total.barrier_summary.warps_released, barrier_queue_rejects: .total.barrier_summary.queue_rejects, barrier_total_wait_cycles: .total.barrier_summary.total_scheduled_wait_cycles, barrier_max_wait_cycles: .total.barrier_summary.max_scheduled_wait_cycles}' \
  "${RUN_DIR}/summary.json"
