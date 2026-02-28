#!/usr/bin/env bash
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DEBUG_OUTPUT=0
case "${1:-}" in
  --debug)
    DEBUG_OUTPUT=1
    ;;
  "")
    ;;
  *)
    echo "Usage: $0 [--debug]"
    exit 2
    ;;
esac

export CYCLOTRON_TIMING_LOG_STATS=1
mkdir -p logs/microbench

if [[ ! -x "target/release/cyclotron" ]]; then
  cargo build --release >/dev/null
fi

CONFIG_TEMPLATE_PATH="test/performance-model-tests/configs/base_template.toml"
CONFIG_MANIFEST_PATH="test/performance-model-tests/configs/manifest.tsv"
OVERRIDE_EMITTER_PATH="test/performance-model-tests/configs/profile_override_emitter.sh"
TMP_CONFIG_ROOT="test/temp_folder"
mkdir -p "$TMP_CONFIG_ROOT"
TMP_CONFIG_DIR="$(mktemp -d "$TMP_CONFIG_ROOT/microbench.XXXXXX")"
cleanup() {
  rm -rf "$TMP_CONFIG_DIR"
  rmdir "$TMP_CONFIG_ROOT" 2>/dev/null || true
}
trap cleanup EXIT

source "$OVERRIDE_EMITTER_PATH"

LATEST_RUN_DIR=""
GENERATED_CONFIG_PATH=""
GENERATED_ELF_PATH=""

render_config_for_key() {
  local key="$1"
  local line
  line="$(awk -F'|' -v k="$key" '$0 !~ /^#/ && NF >= 9 && $1 == k { print; exit }' "$CONFIG_MANIFEST_PATH")"
  if [[ -z "$line" ]]; then
    echo "missing manifest entry for key: $key" >&2
    return 1
  fi

  local manifest_key elf timeout num_lanes num_warps num_cores smem_base base_profiles override_key
  IFS='|' read -r manifest_key elf timeout num_lanes num_warps num_cores smem_base base_profiles override_key <<< "$line"

  local profiles_arr=()
  local base_arr=()
  local base_profile
  IFS=',' read -r -a base_arr <<< "$base_profiles"
  for base_profile in "${base_arr[@]}"; do
    [[ -z "$base_profile" ]] && continue
    profiles_arr+=("../../../config/test_profiles/${base_profile}.toml")
  done

  if [[ "$override_key" != "-" && -n "$override_key" ]]; then
    local override_file="override_${manifest_key}.toml"
    local override_path="$TMP_CONFIG_DIR/$override_file"
    emit_override_profile "$override_key" "$override_path" || return 1
    profiles_arr+=("$override_file")
  fi

  local profiles
  profiles="$(IFS=','; echo "${profiles_arr[*]}")"

  local cfg_path="$TMP_CONFIG_DIR/${manifest_key}.toml"
  awk \
    -v num_lanes="$num_lanes" \
    -v num_warps="$num_warps" \
    -v num_cores="$num_cores" \
    -v elf="$elf" \
    -v timeout="$timeout" \
    -v smem_base="$smem_base" \
    -v profiles="$profiles" '
      {
        gsub(/__NUM_LANES__/, num_lanes);
        gsub(/__NUM_WARPS__/, num_warps);
        gsub(/__NUM_CORES__/, num_cores);
        gsub(/__ELF__/, elf);
        gsub(/__TIMEOUT__/, timeout);

        if ($0 ~ /__SMEM_BASE_ADDR_LINE__/) {
          if (smem_base != "-") {
            print "smem_base_addr = " smem_base;
          }
          next;
        }

        if ($0 ~ /__PROFILE_INCLUDE_LINES__/) {
          n = split(profiles, arr, ",");
          for (i = 1; i <= n; i++) {
            if (length(arr[i]) > 0) {
              printf "  \"%s\",\n", arr[i];
            }
          }
          next;
        }

        print;
      }
    ' "$CONFIG_TEMPLATE_PATH" > "$cfg_path"

  GENERATED_CONFIG_PATH="$cfg_path"
  GENERATED_ELF_PATH="$elf"
}

run_case() {
  local config_key="$1"
  local log_path="$2"

  render_config_for_key "$config_key" || return 1
  local elf_path="test/performance-model-tests/binaries/${GENERATED_ELF_PATH}"
  if [[ ! -f "$elf_path" ]]; then
    echo "missing elf binary: $elf_path" >&2
    return 1
  fi

  if [[ "$DEBUG_OUTPUT" -eq 1 ]]; then
    cargo run --release -- "$GENERATED_CONFIG_PATH" --binary-path "$elf_path" --timing
  else
    cargo run --release -- "$GENERATED_CONFIG_PATH" --binary-path "$elf_path" --timing >"$log_path" 2>&1
  fi

  LATEST_RUN_DIR="$(ls -td performance_logs/run_* 2>/dev/null | sed -n '1p')"
  [[ -n "$LATEST_RUN_DIR" ]]
}

print_snippet() {
  local jq_expr="$1"
  if [[ "$DEBUG_OUTPUT" -eq 1 ]]; then
    echo "Latest run: ${LATEST_RUN_DIR}"
    jq "$jq_expr" "${LATEST_RUN_DIR}/summary.json"
  fi
}

failures=0
tests_run=0

run_test() {
  local title="$1"
  shift
  local fn="$1"
  shift
  tests_run=$((tests_run + 1))

  if [[ "$DEBUG_OUTPUT" -eq 1 ]]; then
    echo "===== ${title} ====="
    if "$fn" "$@"; then
      echo "RESULT: PASS"
    else
      echo "RESULT: FAIL"
      failures=$((failures + 1))
    fi
  else
    printf "%-40s" "${title}"
    if "$fn" "$@"; then
      echo "PASS"
    else
      echo "FAIL"
      failures=$((failures + 1))
    fi
  fi
}

run_basic() {
  local name="$1"
  local config_key="$2"
  local jq_expr="$3"
  local log_path="logs/microbench/${name}.out"
  run_case "$config_key" "$log_path" || return 1
  print_snippet "$jq_expr"
}

run_smem_dual_port() {
  local off_key="smem_dual_port_off"
  local on_key="smem_dual_port_on"
  local off_log="logs/microbench/smem_dual_port_off.out"
  local on_log="logs/microbench/smem_dual_port_on.out"

  run_case "$off_key" "$off_log" || return 1
  local off_run="$LATEST_RUN_DIR"
  local off_bank_total
  off_bank_total="$(jq -r '.total.smem_util.bank_total' "${off_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, smem_conflict_lanes: .total.smem_conflicts.conflict_lanes, smem_unique_banks: .total.smem_conflicts.unique_banks, smem_bank_total: .total.smem_util.bank_total}'

  run_case "$on_key" "$on_log" || return 1
  local on_run="$LATEST_RUN_DIR"
  local on_bank_total
  on_bank_total="$(jq -r '.total.smem_util.bank_total' "${on_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, smem_conflict_lanes: .total.smem_conflicts.conflict_lanes, smem_unique_banks: .total.smem_conflicts.unique_banks, smem_bank_total: .total.smem_util.bank_total}'

  [[ "$off_bank_total" -gt 0 && "$on_bank_total" -eq $((off_bank_total * 2)) ]]
}

run_icache_pair() {
  local name="$1"
  local hit_key="$2"
  local miss_key="$3"
  local hit_log="logs/microbench/${name}_hit.out"
  local miss_log="logs/microbench/${name}_miss.out"

  run_case "$hit_key" "$hit_log" || return 1
  local hit_run="$LATEST_RUN_DIR"
  local hit_cycles hit_issued hit_hits hit_misses
  hit_cycles="$(jq -r '.total.scheduler.cycles' "${hit_run}/summary.json")"
  hit_issued="$(jq -r '.total.icache_stats.issued' "${hit_run}/summary.json")"
  hit_hits="$(jq -r '.total.icache_stats.hits' "${hit_run}/summary.json")"
  hit_misses="$(jq -r '.total.icache_stats.misses' "${hit_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, icache_issued: .total.icache_stats.issued, icache_completed: .total.icache_stats.completed, icache_hits: .total.icache_stats.hits, icache_misses: .total.icache_stats.misses}'

  run_case "$miss_key" "$miss_log" || return 1
  local miss_run="$LATEST_RUN_DIR"
  local miss_cycles miss_issued miss_hits miss_misses
  miss_cycles="$(jq -r '.total.scheduler.cycles' "${miss_run}/summary.json")"
  miss_issued="$(jq -r '.total.icache_stats.issued' "${miss_run}/summary.json")"
  miss_hits="$(jq -r '.total.icache_stats.hits' "${miss_run}/summary.json")"
  miss_misses="$(jq -r '.total.icache_stats.misses' "${miss_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, icache_issued: .total.icache_stats.issued, icache_completed: .total.icache_stats.completed, icache_hits: .total.icache_stats.hits, icache_misses: .total.icache_stats.misses}'

  [[ "$hit_issued" -gt 0 && "$hit_hits" -eq "$hit_issued" && "$hit_misses" -eq 0 && \
     "$miss_issued" -gt 0 && "$miss_misses" -eq "$miss_issued" && "$miss_hits" -eq 0 && \
     "$miss_cycles" -gt "$hit_cycles" ]]
}

run_op_fetch_queue_fill() {
  local baseline_key="op_fetch_queue_fill_baseline"
  local fill_key="op_fetch_queue_fill"
  local baseline_log="logs/microbench/op_fetch_queue_fill_baseline.out"
  local fill_log="logs/microbench/op_fetch_queue_fill_enabled.out"

  run_case "$baseline_key" "$baseline_log" || return 1
  local b_run="$LATEST_RUN_DIR"
  local baseline_cycles
  baseline_cycles="$(jq -r '.total.scheduler.cycles' "${b_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, issued_warps: .total.scheduler.issued_warps_sum, lsu_issued: .total.lsu_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects}'

  run_case "$fill_key" "$fill_log" || return 1
  local f_run="$LATEST_RUN_DIR"
  local fill_cycles
  fill_cycles="$(jq -r '.total.scheduler.cycles' "${f_run}/summary.json")"
  print_snippet '{scheduler_cycles: .total.scheduler.cycles, issued_warps: .total.scheduler.issued_warps_sum, lsu_issued: .total.lsu_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects}'

  [[ "$baseline_cycles" -gt 0 && "$fill_cycles" -gt 0 ]]
}

run_test "execute alu latency" run_basic "execute_alu_latency" "execute_alu_latency" '{cycles: .total.execute_util.cycles, int_busy: .total.execute_util.int_busy_sum, fp_busy: .total.execute_util.fp_busy_sum, sfu_busy: .total.execute_util.sfu_busy_sum}'
run_test "execute fp latency" run_basic "execute_fp_latency" "execute_fp_latency" '{cycles: .total.execute_util.cycles, int_busy: .total.execute_util.int_busy_sum, fp_busy: .total.execute_util.fp_busy_sum, sfu_busy: .total.execute_util.sfu_busy_sum}'
run_test "execute int div latency" run_basic "execute_int_div_latency" "execute_int_div_latency" '{cycles: .total.execute_util.cycles, int_div_busy: .total.execute_util.int_div_busy_sum, fp_busy: .total.execute_util.fp_busy_sum}'
run_test "execute int mul latency" run_basic "execute_int_mul_latency" "execute_int_mul_latency" '{cycles: .total.execute_util.cycles, int_mul_busy: .total.execute_util.int_mul_busy_sum, fp_busy: .total.execute_util.fp_busy_sum}'
run_test "execute sfu latency" run_basic "execute_sfu_latency" "execute_sfu_latency" '{cycles: .total.execute_util.cycles, sfu_busy: .total.execute_util.sfu_busy_sum, issued_warps: .total.scheduler.issued_warps_sum}'

run_test "smem no conflict" run_basic "smem_no_conflict" "smem_no_conflict" '{smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, conflict_lanes: .total.smem_conflicts.conflict_lanes, unique_banks: .total.smem_conflicts.unique_banks}'
run_test "smem partial conflict" run_basic "smem_partial_conflict" "smem_partial_conflict" '{smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, conflict_lanes: .total.smem_conflicts.conflict_lanes, unique_banks: .total.smem_conflicts.unique_banks}'
run_test "smem full conflict" run_basic "smem_full_conflict" "smem_full_conflict" '{smem_issued: .total.smem_stats.issued, smem_completed: .total.smem_stats.completed, conflict_lanes: .total.smem_conflicts.conflict_lanes, unique_banks: .total.smem_conflicts.unique_banks}'
run_test "smem dual port" run_smem_dual_port

run_test "lsu ldq fill" run_basic "lsu_ldq_fill" "lsu_ldq_fill" '{lsu_issued: .total.lsu_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects, gmem_issued: .total.gmem_stats.issued, l0_accesses: .total.gmem_hits.l0_accesses}'
run_test "lsu shared ldq fill" run_basic "lsu_shared_ldq_fill" "lsu_shared_ldq_fill" '{lsu_issued: .total.lsu_stats.issued, shared_ldq_issued: .total.lsu_stats.shared_ldq_issued, smem_issued: .total.smem_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects}'
run_test "lsu stq fill" run_basic "lsu_stq_fill" "lsu_stq_fill" '{lsu_issued: .total.lsu_stats.issued, global_stq_issued: .total.lsu_stats.global_stq_issued, gmem_issued: .total.gmem_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects}'
run_test "lsu shared stq fill" run_basic "lsu_shared_stq_fill" "lsu_shared_stq_fill" '{lsu_issued: .total.lsu_stats.issued, shared_stq_issued: .total.lsu_stats.shared_stq_issued, smem_issued: .total.smem_stats.issued, lsu_queue_full: .total.lsu_stats.queue_full_rejects}'

run_test "gmem bank select" run_basic "gmem_bank_select" "gmem_bank_select" '{issued: .total.gmem_stats.issued, completed: .total.gmem_stats.completed, l0_accesses: .total.gmem_hits.l0_accesses}'
run_test "gmem coalesce" run_basic "gmem_coalesce" "gmem_coalesce" '{issued: .total.gmem_stats.issued, l1_accesses: .total.gmem_hits.l1_accesses, l2_accesses: .total.gmem_hits.l2_accesses}'
run_test "gmem l0 hit" run_basic "gmem_l0_hit" "gmem_l0_hit" '{issued: .total.gmem_stats.issued, l0_accesses: .total.gmem_hits.l0_accesses, l0_hits: .total.gmem_hits.l0_hits}'
run_test "gmem l1 miss" run_basic "gmem_l1_miss" "gmem_l1_miss" '{issued: .total.gmem_stats.issued, l1_accesses: .total.gmem_hits.l1_accesses, l1_hits: .total.gmem_hits.l1_hits, l2_accesses: .total.gmem_hits.l2_accesses}'
run_test "gmem l2 miss" run_basic "gmem_l2_miss" "gmem_l2_miss" '{issued: .total.gmem_stats.issued, l2_accesses: .total.gmem_hits.l2_accesses, l2_hits: .total.gmem_hits.l2_hits}'
run_test "gmem mshr fill" run_basic "gmem_mshr_fill" "gmem_mshr_fill" '{issued: .total.gmem_stats.issued, queue_full_rejects: .total.gmem_stats.queue_full_rejects, busy_rejects: .total.gmem_stats.busy_rejects}'
run_test "gmem mshr merge" run_basic "gmem_mshr_merge" "gmem_mshr_merge" '{issued: .total.gmem_stats.issued, queue_full_rejects: .total.gmem_stats.queue_full_rejects, busy_rejects: .total.gmem_stats.busy_rejects}'

run_test "icache small loop" run_icache_pair "icache_small_loop" "icache_small_loop_hit" "icache_small_loop_miss"
run_test "icache large loop" run_icache_pair "icache_large_loop" "icache_large_loop_hit" "icache_large_loop_miss"
run_test "icache thrash" run_icache_pair "icache_thrash" "icache_thrash_hit" "icache_thrash_miss"

run_test "op fetch queue fill" run_op_fetch_queue_fill

run_test "writeback gmem saturate" run_basic "writeback_gmem_saturate" "writeback_gmem_saturate" '{writeback_issued: .total.writeback_stats.issued, writeback_completed: .total.writeback_stats.completed, writeback_queue_full: .total.writeback_stats.queue_full_rejects, gmem_completed: .total.gmem_stats.completed}'
run_test "writeback smem saturate" run_basic "writeback_smem_saturate" "writeback_smem_saturate" '{writeback_issued: .total.writeback_stats.issued, writeback_completed: .total.writeback_stats.completed, writeback_queue_full: .total.writeback_stats.queue_full_rejects, smem_completed: .total.smem_stats.completed}'
run_test "writeback mixed saturate" run_basic "writeback_mixed_saturate" "writeback_mixed_saturate" '{writeback_issued: .total.writeback_stats.issued, writeback_completed: .total.writeback_stats.completed, writeback_queue_full: .total.writeback_stats.queue_full_rejects, gmem_completed: .total.gmem_stats.completed, smem_completed: .total.smem_stats.completed}'

if [[ "$tests_run" -ne 27 ]]; then
  echo "Internal error: expected 27 tests, ran ${tests_run}"
  exit 2
fi

if [[ "$failures" -ne 0 ]]; then
  echo "${failures} microbenchmark tests failed"
  exit 1
fi

echo "all 27 microbenchmark tests passed"
