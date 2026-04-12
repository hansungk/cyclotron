#!/usr/bin/env python3
"""Run Cyclotron SMEM traffic frontend and compare checkpoints to an RTL .out file.

Usage:
  python3 tuning_scripts/compare_smem_run.py /path/to/smem_rtl_output1.out
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

    proc = subprocess.run(
        cmd,
        cwd=cyclotron_root,
        capture_output=True,
        text=True,
    )

    combined_output = f"{proc.stdout}\n{proc.stderr}"
    checkpoints = parse_checkpoints_from_text(combined_output)

    if proc.returncode != 0:
        print(combined_output)
        raise RuntimeError(f"Cyclotron run failed with exit code {proc.returncode}")

    if not checkpoints:
        print(combined_output)
        raise RuntimeError("Cyclotron run completed but no [TRAFFIC] checkpoints were found")

    return checkpoints


def align_checkpoints(
    rtl: list[Checkpoint], cyclotron: list[Checkpoint]
) -> list[tuple[str, int, int, int, int]]:
    if len(rtl) != len(cyclotron):
        raise RuntimeError(
            f"Checkpoint count mismatch: RTL={len(rtl)}, Cyclotron={len(cyclotron)}"
        )

    rows: list[tuple[str, int, int, int, int]] = []
    accumulated_abs_diff = 0
    prev_rtl_cycle = 0
    prev_cyclotron_cycle = 0

    for idx, (rt, cy) in enumerate(zip(rtl, cyclotron)):
        if rt.pattern != cy.pattern:
            raise RuntimeError(
                "Pattern mismatch at index "
                f"{idx}: RTL='{rt.pattern}', Cyclotron='{cy.pattern}'"
            )

        rtl_pattern_cycles = rt.cycle - prev_rtl_cycle
        cyclotron_pattern_cycles = cy.cycle - prev_cyclotron_cycle
        diff = cyclotron_pattern_cycles - rtl_pattern_cycles
        accumulated_abs_diff += abs(diff)

        rows.append((rt.pattern, rt.cycle, cy.cycle, diff, accumulated_abs_diff))
        prev_rtl_cycle = rt.cycle
        prev_cyclotron_cycle = cy.cycle
    return rows


def print_table(rows: list[tuple[str, int, int, int, int]]) -> None:
    pattern_w = max(len("pattern"), *(len(r[0]) for r in rows))

    print(
        f"{'pattern':<{pattern_w}}  "
        f"{'rtl_cycle':>12}  "
        f"{'cyclotron_cycle':>15}  "
        f"{'diff':>10}  "
        f"{'accumulated_abs_diff':>20}"
    )
    print("-" * (pattern_w + 2 + 12 + 2 + 15 + 2 + 10 + 2 + 20))

    for pattern, rtl_cycle, cyclotron_cycle, diff, accumulated_abs_diff in rows:
        print(
            f"{pattern:<{pattern_w}}  "
            f"{rtl_cycle:>12}  "
            f"{cyclotron_cycle:>15}  "
            f"{diff:>10}  "
            f"{accumulated_abs_diff:>20}"
        )


def print_overall_error(rows: list[tuple[str, int, int, int, int]]) -> None:
    diffs = [r[3] for r in rows]
    abs_diffs = [abs(d) for d in diffs]

    mae = sum(abs_diffs) / len(abs_diffs)
    rmse = math.sqrt(sum(d * d for d in diffs) / len(diffs))
    max_abs = max(abs_diffs)
    total_abs = sum(abs_diffs)

    print("\noverall_error")
    print(f"{'mae':<16}{mae:>12.3f}")
    print(f"{'rmse':<16}{rmse:>12.3f}")
    print(f"{'max_abs_diff':<16}{max_abs:>12}")
    print(f"{'total_abs_diff':<16}{total_abs:>12}")


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
    args = parser.parse_args()

    rtl_out = args.rtl_out.resolve()
    if not rtl_out.exists():
        print(f"RTL .out file not found: {rtl_out}", file=sys.stderr)
        return 1

    rtl = parse_rtl_out(rtl_out)
    cyclotron = run_cyclotron(cyclotron_root, args.sim_config, args.traffic_config)
    rows = align_checkpoints(rtl, cyclotron)

    print_table(rows)
    print_overall_error(rows)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
