# Cyclotron

Cyclotron is a performance model for the Radiance GPU architecture. It has a functional RISC-V SIMT simulator, and a timing layer that runs on top of it.

## Usage

### Building

```bash
cargo build --release
```

### Basic execution (functional only)

```bash
cargo run --release -- config.toml
```

Or with a specific binary:

```bash
cargo run --release -- config.toml --binary-path path/to/kernel.elf
```

### With timing model enabled

```bash
cargo run --release -- config.toml --timing
```

```bash
cargo run --release -- config.toml --binary-path path/to/kernel.elf --timing
```

```bash
CYCLOTRON_GRAPH_LOG=1 cargo run --release -- config.toml --timing --binary-path  path/to/kernel.elf
```

### Common CLI options

| Option | Description |
|--------|-------------|
| `--timing` | Enable the timing model |
| `--binary-path <path>` | Override the ELF binary to run |
| `--num-lanes <N>` | Override lanes per warp (default: 16) |
| `--num-warps <N>` | Override warps per core (default: 4) |
| `--num-cores <N>` | Override cores per cluster (default: 1) |
| `--log <level>` | Log level: 0=none, 1=info, 2=debug |
| `--gen-trace <bool>` | Generate instruction trace |

### Example: Run ISA tests

```bash
./test/run-isa-tests.sh
```

### Example: Run microbenchmarks with timing

```bash
./test/run-microbench-tests.sh
```

## Performance Logging

When the timing model is enabled, Cyclotron automatically writes performance logs to `performance_logs/run_<timestamp>_<pid>/`.

### Environment variables

| Variable | Description |
|----------|-------------|
| `CYCLOTRON_PERF_LOG_DIR` | Override the base directory for logs (default: `performance_logs/`) |
| `CYCLOTRON_GRAPH_LOG=1` | Enable detailed backpressure event logging |

### Output files

Each run directory contains:

| File | Description |
|------|-------------|
| `summary.json` | **End-of-run aggregate statistics** — the primary output. Contains per-core and total metrics for scheduler utilization, cache hit rates, memory latencies, SMEM bank conflicts, LSU statistics, and more. |
| `stats.jsonl` | **Per-cycle statistics stream**. Each line is a snapshot of core performance counters at a given cycle. |
| `graph_backpressure.jsonl` | **Backpressure events** (only if `CYCLOTRON_GRAPH_LOG=1`). Logs every rejected request in the FlowGraph: which edge, source/destination nodes, rejection reason, retry cycle, and queue capacity. |


Timing parameters are modular — see `config/timing/` for individual component configurations (cache sizes, queue depths, latencies, etc.).

## Tuning Parameters

### SMEM traffic workflow 

Generate a Cyclotron traffic config directly from a Radiance `.out` reference:

```bash
./scripts/generate_smem_traffic_toml.py \
  --reference-out ../radiance/none.smem_test.out \
  --output config/traffic/smem_radiance.toml
```

Run STF (synthetic traffic frontend) mode and compare against the same reference:

```bash
./scripts/validate_smem_parity.py \
  --reference-out ../radiance/none.smem_test.out
```