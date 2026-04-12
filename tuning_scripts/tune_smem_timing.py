#!/usr/bin/env python3
"""Auto-tune SMEM timing parameters against an RTL .out checkpoint file.

This tunes only these fields in config/timing/smem.toml:
- base_overhead
- read_extra (or extra_read_latency, if present)

Usage:
  python3 tuning_scripts/tune_smem_timing.py /path/to/smem_rtl_output1.out
"""

from __future__ import annotations

import argparse
import math
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


TRAFFIC_RE = re.compile(r"\[TRAFFIC\]\s+core\s+\d+\s+(.+)\s+finished at time\s+(\d+)")


@dataclass
class Checkpoint:
    pattern: str
    cycle: int


@dataclass
class Params:
    base_overhead: int
    read_latency: int
    read_key: str


def parse_checkpoints_from_text(text: str) -> list[Checkpoint]:
    checkpoints: list[Checkpoint] = []
    for line in text.splitlines():
        match = TRAFFIC_RE.search(line)
        if not match:
            continue
        checkpoints.append(Checkpoint(pattern=match.group(1), cycle=int(match.group(2))))
    return checkpoints


def parse_rtl_out(path: Path) -> list[Checkpoint]:
    return parse_checkpoints_from_text(path.read_text())


def run_cyclotron(cyclotron_root: Path, sim_config: Path, traffic_config: Path) -> list[Checkpoint]:
    cmd = [
        "cargo",
        "run",
        "--release",
        "--",
        str(sim_config),
        "--timing",
        "--frontend-mode",
        "traffic_smem",
        "--traffic-config",
        str(traffic_config),
    ]

    proc = subprocess.run(cmd, cwd=cyclotron_root, capture_output=True, text=True)
    combined_output = f"{proc.stdout}\n{proc.stderr}"
    checkpoints = parse_checkpoints_from_text(combined_output)

    if proc.returncode != 0:
        print(combined_output)
        raise RuntimeError(f"Cyclotron run failed with exit code {proc.returncode}")

    if not checkpoints:
        print(combined_output)
        raise RuntimeError("Cyclotron run completed but no [TRAFFIC] checkpoints were found")

    return checkpoints


def ensure_aligned(rtl: list[Checkpoint], cyclotron: list[Checkpoint]) -> None:
    if len(rtl) != len(cyclotron):
        raise RuntimeError(
            f"Checkpoint count mismatch: RTL={len(rtl)}, Cyclotron={len(cyclotron)}"
        )

    for idx, (rt, cy) in enumerate(zip(rtl, cyclotron)):
        if rt.pattern != cy.pattern:
            raise RuntimeError(
                f"Pattern mismatch at index {idx}: RTL='{rt.pattern}', Cyclotron='{cy.pattern}'"
            )


def finished_to_durations(cycles: list[int]) -> list[int]:
    if not cycles:
        return []
    durations = [cycles[0]]
    for idx in range(1, len(cycles)):
        durations.append(cycles[idx] - cycles[idx - 1])
    return durations


def error_stats(reference: list[float], estimate: list[float]) -> tuple[float, float, int]:
    diffs = [est - ref for ref, est in zip(reference, estimate)]
    abs_diffs = [abs(d) for d in diffs]
    mae = sum(abs_diffs) / len(abs_diffs)
    rmse = math.sqrt(sum(d * d for d in diffs) / len(diffs))
    max_abs = int(max(abs_diffs))
    return mae, rmse, max_abs


def extract_int_param(text: str, key: str) -> int | None:
    match = re.search(rf"^\s*{re.escape(key)}\s*=\s*(\d+)\b", text, re.MULTILINE)
    if not match:
        return None
    return int(match.group(1))


def load_smem_params(smem_toml: Path) -> Params:
    text = smem_toml.read_text()

    base_overhead = extract_int_param(text, "base_overhead")
    if base_overhead is None:
        raise RuntimeError(f"base_overhead not found in {smem_toml}")

    read_key = "extra_read_latency" if "extra_read_latency" in text else "read_extra"
    read_latency = extract_int_param(text, read_key)
    if read_latency is None:
        raise RuntimeError(f"{read_key} not found in {smem_toml}")

    return Params(base_overhead=base_overhead, read_latency=read_latency, read_key=read_key)


def replace_int_param(text: str, key: str, value: int) -> str:
    pattern = re.compile(
        rf"^(\s*{re.escape(key)}\s*=\s*)(\d+)(\s*(?:#.*)?)$",
        re.MULTILINE,
    )
    new_text, replaced = pattern.subn(rf"\g<1>{int(value)}\g<3>", text, count=1)
    if replaced != 1:
        raise RuntimeError(f"Failed to update '{key}' in smem.toml")
    return new_text


def write_smem_params(smem_toml: Path, base_overhead: int, read_key: str, read_latency: int) -> None:
    text = smem_toml.read_text()
    text = replace_int_param(text, "base_overhead", base_overhead)
    text = replace_int_param(text, read_key, read_latency)
    smem_toml.write_text(text)


def solve_least_squares(
    rtl_durations: list[int],
    base_durations: list[int],
    read_flags: list[int],
    base_overhead_0: int,
    read_latency_0: int,
) -> tuple[float, float]:
    # Build y = raw + base_overhead + read_latency * read_flag
    # raw is extracted from the baseline run.
    raw = [
        d0 - base_overhead_0 - read_latency_0 * rf
        for d0, rf in zip(base_durations, read_flags)
    ]
    target = [yr - rr for yr, rr in zip(rtl_durations, raw)]

    n = len(target)
    sx = float(sum(read_flags))
    sx2 = float(sum(rf * rf for rf in read_flags))
    sy = float(sum(target))
    sxy = float(sum(t * rf for t, rf in zip(target, read_flags)))

    det = n * sx2 - sx * sx
    if abs(det) < 1e-12:
        # Degenerate case; keep read_latency unchanged, tune base by mean residual.
        base_hat = sy / n
        read_hat = float(read_latency_0)
        return base_hat, read_hat

    base_hat = (sy * sx2 - sxy * sx) / det
    read_hat = (n * sxy - sx * sy) / det
    return base_hat, read_hat


def find_best_integer_pair(
    rtl_durations: list[int],
    base_durations: list[int],
    read_flags: list[int],
    base_overhead_0: int,
    read_latency_0: int,
    base_current: int,
    read_current: int,
    base_hat: float,
    read_hat: float,
) -> tuple[int, int, float, float, int]:
    raw = [
        d0 - base_overhead_0 - read_latency_0 * rf
        for d0, rf in zip(base_durations, read_flags)
    ]

    b_center = int(round(base_hat))
    r_center = int(round(read_hat))
    b_lo = max(0, min(base_current, b_center) - 8)
    b_hi = max(base_current, b_center) + 8
    r_lo = max(0, min(read_current, r_center) - 8)
    r_hi = max(read_current, r_center) + 8

    best = None
    for b in range(b_lo, b_hi + 1):
        for r in range(r_lo, r_hi + 1):
            pred_durations = [rr + b + r * rf for rr, rf in zip(raw, read_flags)]
            mae, rmse, max_abs = error_stats(
                [float(x) for x in rtl_durations],
                pred_durations,
            )
            distance_from_current = abs(b - base_current) + abs(r - read_current)
            # Optimize for per-pattern absolute error first, then RMSE.
            score = (mae, rmse, max_abs, distance_from_current)
            cand = (b, r, mae, rmse, max_abs, score)
            if best is None or cand[-1] < best[-1]:
                best = cand

    assert best is not None
    return best[0], best[1], best[2], best[3], best[4]


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    cyclotron_root = script_dir.parent

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rtl_out", type=Path, help="Path to RTL .out file")
    parser.add_argument(
        "--sim-config",
        type=Path,
        default=cyclotron_root / "config.toml",
        help="Cyclotron sim config path (default: config.toml)",
    )
    parser.add_argument(
        "--traffic-config",
        type=Path,
        default=cyclotron_root / "config/traffic/smem_radiance.toml",
        help="Traffic config path (default: config/traffic/smem_radiance.toml)",
    )
    parser.add_argument(
        "--smem-toml",
        type=Path,
        default=cyclotron_root / "config/timing/smem.toml",
        help="SMEM timing TOML to update (default: config/timing/smem.toml)",
    )
    args = parser.parse_args()

    rtl_out = args.rtl_out.resolve()
    if not rtl_out.exists():
        print(f"RTL .out file not found: {rtl_out}", file=sys.stderr)
        return 1

    smem_toml = args.smem_toml.resolve()
    if not smem_toml.exists():
        print(f"smem.toml not found: {smem_toml}", file=sys.stderr)
        return 1

    rtl = parse_rtl_out(rtl_out)
    baseline = run_cyclotron(cyclotron_root, args.sim_config, args.traffic_config)
    ensure_aligned(rtl, baseline)

    params_before = load_smem_params(smem_toml)

    rtl_cycles = [cp.cycle for cp in rtl]
    base_cycles = [cp.cycle for cp in baseline]
    rtl_durations = finished_to_durations(rtl_cycles)
    base_durations = finished_to_durations(base_cycles)
    read_flags = [1 if cp.pattern.endswith("_r") else 0 for cp in rtl]

    mae_before_dur, rmse_before_dur, max_abs_before_dur = error_stats(
        [float(x) for x in rtl_durations],
        [float(x) for x in base_durations],
    )
    mae_before_cyc, rmse_before_cyc, max_abs_before_cyc = error_stats(
        [float(x) for x in rtl_cycles],
        [float(x) for x in base_cycles],
    )

    base_hat, read_hat = solve_least_squares(
        rtl_durations=rtl_durations,
        base_durations=base_durations,
        read_flags=read_flags,
        base_overhead_0=params_before.base_overhead,
        read_latency_0=params_before.read_latency,
    )

    best_base, best_read, pred_mae, pred_rmse, pred_max_abs = find_best_integer_pair(
        rtl_durations=rtl_durations,
        base_durations=base_durations,
        read_flags=read_flags,
        base_overhead_0=params_before.base_overhead,
        read_latency_0=params_before.read_latency,
        base_current=params_before.base_overhead,
        read_current=params_before.read_latency,
        base_hat=base_hat,
        read_hat=read_hat,
    )

    write_smem_params(
        smem_toml=smem_toml,
        base_overhead=best_base,
        read_key=params_before.read_key,
        read_latency=best_read,
    )

    after = run_cyclotron(cyclotron_root, args.sim_config, args.traffic_config)
    ensure_aligned(rtl, after)
    after_cycles = [cp.cycle for cp in after]
    after_durations = finished_to_durations(after_cycles)
    mae_after_dur, rmse_after_dur, max_abs_after_dur = error_stats(
        [float(x) for x in rtl_durations],
        [float(x) for x in after_durations],
    )
    mae_after_cyc, rmse_after_cyc, max_abs_after_cyc = error_stats(
        [float(x) for x in rtl_cycles],
        [float(x) for x in after_cycles],
    )

    print("tuning_result")
    print(f"{'param':<24}{'before':>12}{'after':>12}")
    print(f"{'base_overhead':<24}{params_before.base_overhead:>12}{best_base:>12}")
    print(f"{params_before.read_key:<24}{params_before.read_latency:>12}{best_read:>12}")

    print("\nerror_summary_duration")
    print(f"{'metric':<24}{'before':>12}{'after':>12}{'pred_after':>12}")
    print(f"{'mae':<24}{mae_before_dur:>12.3f}{mae_after_dur:>12.3f}{pred_mae:>12.3f}")
    print(f"{'rmse':<24}{rmse_before_dur:>12.3f}{rmse_after_dur:>12.3f}{pred_rmse:>12.3f}")
    print(f"{'max_abs_diff':<24}{max_abs_before_dur:>12}{max_abs_after_dur:>12}{pred_max_abs:>12}")

    print("\nerror_summary_finished_cycles")
    print(f"{'metric':<24}{'before':>12}{'after':>12}")
    print(f"{'mae':<24}{mae_before_cyc:>12.3f}{mae_after_cyc:>12.3f}")
    print(f"{'rmse':<24}{rmse_before_cyc:>12.3f}{rmse_after_cyc:>12.3f}")
    print(f"{'max_abs_diff':<24}{max_abs_before_cyc:>12}{max_abs_after_cyc:>12}")

    print(f"\nUpdated: {smem_toml}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
