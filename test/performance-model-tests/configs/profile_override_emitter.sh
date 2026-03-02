#!/usr/bin/env bash

emit_override_profile() {
  local key="$1"
  local out_path="$2"

  case "$key" in
    -|none|"")
      : > "$out_path"
      ;;

    execute_alu_latency)
      cat > "$out_path" <<'EOT'
[execute.alu]
base_latency = 12
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[execute.int_mul]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_div]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.fp]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.sfu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    execute_fp_latency)
      cat > "$out_path" <<'EOT'
[execute.alu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_mul]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_div]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.fp]
base_latency = 12
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[execute.sfu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    execute_int_div_latency)
      cat > "$out_path" <<'EOT'
[execute.alu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_mul]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_div]
base_latency = 20
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[execute.fp]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.sfu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    execute_int_mul_latency)
      cat > "$out_path" <<'EOT'
[execute.alu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_mul]
base_latency = 12
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[execute.int_div]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.fp]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.sfu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    execute_sfu_latency)
      cat > "$out_path" <<'EOT'
[execute.alu]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_mul]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.int_div]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.fp]
base_latency = 0
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[execute.sfu]
base_latency = 12
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1
EOT
      ;;

    smem_no_conflict)
      cat > "$out_path" <<'EOT'
[smem]
num_banks = 16
num_lanes = 16
num_subbanks = 1
word_bytes = 4
serialize_cores = false
link_capacity = 1024

[smem.lane]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.serial]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.crossbar]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.subbank]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.bank]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    smem_partial_conflict)
      cat > "$out_path" <<'EOT'
[smem]
num_banks = 8
num_lanes = 16
num_subbanks = 1
word_bytes = 4
serialize_cores = false
link_capacity = 1024

[smem.lane]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.serial]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.crossbar]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.subbank]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024

[smem.bank]
base_latency = 1
bytes_per_cycle = 1024
queue_capacity = 1024
completions_per_cycle = 1024
EOT
      ;;
    smem_dual_port_off)
      cat > "$out_path" <<'EOT'
[smem]
num_banks = 8
num_lanes = 16
num_subbanks = 2
word_bytes = 4
dual_port = false
serialize_cores = false
link_capacity = 1024

[smem.lane]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.serial]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.crossbar]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.subbank]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.bank]
base_latency = 20
bytes_per_cycle = 4
queue_capacity = 4096
completions_per_cycle = 1
EOT
      ;;
    smem_dual_port_on)
      cat > "$out_path" <<'EOT'
[smem]
num_banks = 8
num_lanes = 16
num_subbanks = 2
word_bytes = 4
dual_port = true
serialize_cores = false
link_capacity = 1024

[smem.lane]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.serial]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.crossbar]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.subbank]
base_latency = 1
bytes_per_cycle = 65536
queue_capacity = 65536
completions_per_cycle = 65536

[smem.bank]
base_latency = 20
bytes_per_cycle = 4
queue_capacity = 4096
completions_per_cycle = 1
EOT
      ;;

    lsu_ldq_fill)
      cat > "$out_path" <<'EOT'
[lsu.queues.global_ldq]
queue_capacity = 2

[lsu.queues.global_stq]
queue_capacity = 64

[lsu.queues.shared_ldq]
queue_capacity = 64

[lsu.queues.shared_stq]
queue_capacity = 64

[lsu.issue]
queue_capacity = 1
completions_per_cycle = 1
bytes_per_cycle = 1
EOT
      ;;
    lsu_shared_ldq_fill)
      cat > "$out_path" <<'EOT'
[lsu.queues.global_ldq]
queue_capacity = 64

[lsu.queues.global_stq]
queue_capacity = 64

[lsu.queues.shared_ldq]
queue_capacity = 2

[lsu.queues.shared_stq]
queue_capacity = 64

[lsu.issue]
queue_capacity = 1
completions_per_cycle = 1
bytes_per_cycle = 1
EOT
      ;;
    lsu_stq_fill)
      cat > "$out_path" <<'EOT'
[lsu.queues.global_ldq]
queue_capacity = 64

[lsu.queues.global_stq]
queue_capacity = 2

[lsu.queues.shared_ldq]
queue_capacity = 64

[lsu.queues.shared_stq]
queue_capacity = 64

[lsu.issue]
queue_capacity = 1
completions_per_cycle = 1
bytes_per_cycle = 1
EOT
      ;;
    lsu_shared_stq_fill)
      cat > "$out_path" <<'EOT'
[lsu.queues.global_ldq]
queue_capacity = 64

[lsu.queues.global_stq]
queue_capacity = 64

[lsu.queues.shared_ldq]
queue_capacity = 64

[lsu.queues.shared_stq]
queue_capacity = 2

[lsu.issue]
queue_capacity = 1
completions_per_cycle = 1
bytes_per_cycle = 1
EOT
      ;;

    gmem_l0_hit)
      cat > "$out_path" <<'EOT'
[gmem.stats_range]
start = 0x90000000
end = 0x90000040
EOT
      ;;
    gmem_l1_miss_range)
      cat > "$out_path" <<'EOT'
[gmem.stats_range]
start = 0x90000000
end = 0x90000040
EOT
      ;;
    gmem_l2_miss_range)
      cat > "$out_path" <<'EOT'
[gmem.stats_range]
start = 0x90000000
end = 0x90000100
EOT
      ;;
    gmem_mshr_fill_range)
      cat > "$out_path" <<'EOT'
[gmem.stats_range]
start = 0x90000000
end = 0x90000400
EOT
      ;;
    gmem_mshr_merge_range)
      cat > "$out_path" <<'EOT'
[gmem.stats_range]
start = 0x90000000
end = 0x90000040
EOT
      ;;

    icache_hit)
      cat > "$out_path" <<'EOT'
[icache.policy]
hit_rate = 1.0
line_bytes = 32
seed = 0

[icache.hit]
base_latency = 1
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[icache.miss]
base_latency = 40
bytes_per_cycle = 1
queue_capacity = 4
completions_per_cycle = 1
EOT
      ;;
    icache_miss)
      cat > "$out_path" <<'EOT'
[icache.policy]
hit_rate = 0.0
line_bytes = 32
seed = 0

[icache.hit]
base_latency = 1
bytes_per_cycle = 1
queue_capacity = 64
completions_per_cycle = 1

[icache.miss]
base_latency = 40
bytes_per_cycle = 1
queue_capacity = 4
completions_per_cycle = 1
EOT
      ;;

    op_fetch_queue_fill)
      cat > "$out_path" <<'EOT'
[operand_fetch]
enabled = true
base_latency = 64
bytes_per_cycle = 4
queue_capacity = 2
completions_per_cycle = 1

[gmem.stats_range]
start = 0x90000000
end = 0x90000040
EOT
      ;;
    op_fetch_queue_fill_baseline)
      cat > "$out_path" <<'EOT'
[operand_fetch]
enabled = false
EOT
      ;;

    writeback_saturate)
      cat > "$out_path" <<'EOT'
[writeback]
enabled = true
base_latency = 6
bytes_per_cycle = 4
queue_capacity = 2
completions_per_cycle = 1

[gmem.policy]
l0_hit_rate = 1.0
l1_hit_rate = 1.0
l2_hit_rate = 1.0
l1_writeback_rate = 0.0
l2_writeback_rate = 0.0
EOT
      ;;

    *)
      echo "unknown override profile key: $key" >&2
      return 1
      ;;
  esac
}
