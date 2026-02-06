#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export CYCLOTRON_TIMING_LOG_STATS=1

cd "$ROOT_DIR"

ELF_PATH="test/performance-model-tests/binaries/icache_thrash.elf"
HIT_CFG="test/performance-model-tests/configs/icache_thrash_hit.toml"
MISS_CFG="test/performance-model-tests/configs/icache_thrash_miss.toml"

run_case() {
  local label="$1"
  local cfg="$2"
  local prefix="$3"

  cargo run --release -- "$cfg" --binary-path "$ELF_PATH"
  local run_dir
  run_dir="$(ls -td performance_logs/run_* | head -n 1)"

  local cycles issued completed hits misses qf busy
  cycles="$(jq -r '.total.scheduler.cycles' "${run_dir}/summary.json")"
  issued="$(jq -r '.total.icache_stats.issued' "${run_dir}/summary.json")"
  completed="$(jq -r '.total.icache_stats.completed' "${run_dir}/summary.json")"
  hits="$(jq -r '.total.icache_stats.hits' "${run_dir}/summary.json")"
  misses="$(jq -r '.total.icache_stats.misses' "${run_dir}/summary.json")"
  qf="$(jq -r '.total.icache_stats.queue_full_rejects' "${run_dir}/summary.json")"
  busy="$(jq -r '.total.icache_stats.busy_rejects' "${run_dir}/summary.json")"

  echo "Latest run (${label}): ${run_dir}"
  jq '{scheduler_cycles: .total.scheduler.cycles, icache_issued: .total.icache_stats.issued, icache_completed: .total.icache_stats.completed, icache_hits: .total.icache_stats.hits, icache_misses: .total.icache_stats.misses, icache_queue_full: .total.icache_stats.queue_full_rejects, icache_busy: .total.icache_stats.busy_rejects}' "${run_dir}/summary.json"

  printf -v "${prefix}_cycles" '%s' "$cycles"
  printf -v "${prefix}_issued" '%s' "$issued"
  printf -v "${prefix}_completed" '%s' "$completed"
  printf -v "${prefix}_hits" '%s' "$hits"
  printf -v "${prefix}_misses" '%s' "$misses"
  printf -v "${prefix}_qf" '%s' "$qf"
  printf -v "${prefix}_busy" '%s' "$busy"
}

hit_cycles=0
hit_issued=0
hit_completed=0
hit_hits=0
hit_misses=0
hit_qf=0
hit_busy=0

miss_cycles=0
miss_issued=0
miss_completed=0
miss_hits=0
miss_misses=0
miss_qf=0
miss_busy=0

run_case "all_hit" "$HIT_CFG" hit
run_case "all_miss" "$MISS_CFG" miss

if [[ "$hit_issued" -gt 0 && "$hit_issued" -eq "$hit_completed" && "$hit_hits" -eq "$hit_issued" && "$hit_misses" -eq 0 && "$miss_issued" -gt 0 && "$miss_issued" -eq "$miss_completed" && "$miss_misses" -eq "$miss_issued" && "$miss_hits" -eq 0 && "$miss_cycles" -gt "$hit_cycles" ]]; then
  echo "Icache thrash check: PASS"
  echo "  hit_cycles=${hit_cycles}"
  echo "  miss_cycles=${miss_cycles}"
else
  echo "Icache thrash check: FAIL"
  echo "  hit: issued=${hit_issued} completed=${hit_completed} hits=${hit_hits} misses=${hit_misses} cycles=${hit_cycles} qf=${hit_qf} busy=${hit_busy}"
  echo "  miss: issued=${miss_issued} completed=${miss_completed} hits=${miss_hits} misses=${miss_misses} cycles=${miss_cycles} qf=${miss_qf} busy=${miss_busy}"
  exit 1
fi
