#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"
CONFIG_PATH="test/performance-model-tests/configs/execute_alu_latency.toml"
ELF_PATH="test/performance-model-tests/binaries/execute_alu_latency.elf"

cargo run --release -- "$CONFIG_PATH" --binary-path "$ELF_PATH"
RUN_DIR="$(ls -td performance_logs/run_* | head -n 1)"
echo "Latest run: ${RUN_DIR}"
jq '{cycles: .total.execute_util.cycles, int_busy: .total.execute_util.int_busy_sum, int_mul_busy: .total.execute_util.int_mul_busy_sum, int_div_busy: .total.execute_util.int_div_busy_sum, fp_busy: .total.execute_util.fp_busy_sum, sfu_busy: .total.execute_util.sfu_busy_sum, issued_warps: .total.scheduler.issued_warps_sum}' "${RUN_DIR}/summary.json"
