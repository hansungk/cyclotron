#!/usr/bin/env python3
"""Compare Cyclotron SMEM traffic frontend output against RTL stimulus logs.

By default this scans ../tuning_files/smem, ignores stale/non-tuning logs such
as smem_gemmini_synth.out, and prints one comparison table per RTL .out file.
"""

from __future__ import annotations

import argparse
import math
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

from tune_smem_primitives import (
    USEFUL_LOGS,
    StimRow,
    align,
    discover_logs,
    parse_out,
    score_rows,
    write_traffic_config,
)


@dataclass(frozen=True)
class FileComparison:
    source: Path
    rows: list[tuple[StimRow, StimRow, int]]


def parse_cyclotron_rows(text: str, source: str) -> list[StimRow]:
    from tune_smem_primitives import parse_rows

    return parse_rows(text, source)


def run_cyclotron(
    cyclotron_root: Path,
    sim_config: Path,
    traffic_config: Path,
    run_timeout_seconds: int,
) -> list[StimRow]:
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
        timeout=run_timeout_seconds,
    )
    output = f"{proc.stdout}\n{proc.stderr}"
    rows = parse_cyclotron_rows(output, traffic_config.name)
    if proc.returncode != 0:
        print(output)
        raise RuntimeError(f"Cyclotron failed with exit code {proc.returncode}")
    if not rows:
        print(output)
        raise RuntimeError("Cyclotron emitted no SMEM [STIM] traffic rows")
    return rows


def pct_diff(diff: int, rtl_duration: int) -> float:
    if rtl_duration == 0:
        return math.nan
    return 100.0 * diff / rtl_duration


def format_pct(value: float) -> str:
    if math.isnan(value):
        return "na"
    return f"{value:.2f}%"


def compare_file(
    cyclotron_root: Path,
    sim_config: Path,
    rtl_out: Path,
    tmpdir: Path,
    run_timeout_seconds: int,
) -> FileComparison:
    rtl_rows = parse_out(rtl_out)
    traffic_config = tmpdir / f"{rtl_out.stem}.traffic.toml"
    write_traffic_config(traffic_config, rtl_rows)
    cyclotron_rows = run_cyclotron(
        cyclotron_root,
        sim_config,
        traffic_config,
        run_timeout_seconds,
    )
    return FileComparison(rtl_out, align(rtl_rows, cyclotron_rows))


def print_file_table(comparison: FileComparison) -> None:
    rows = comparison.rows
    score = score_rows(rows)
    suite_w = max(len("suite"), *(len(rtl.suite) for rtl, _cyc, _diff in rows))
    pattern_w = max(len("pattern"), *(len(rtl.pattern) for rtl, _cyc, _diff in rows))

    print("")
    print(f"file: {comparison.source.name}")
    print(
        f"summary: count={score.count} mae={score.mae:.3f} "
        f"rmse={score.rmse:.3f} max_abs={score.max_abs} total_abs={score.total_abs}"
    )
    print(
        f"{'suite':<{suite_w}}  "
        f"{'pattern':<{pattern_w}}  "
        f"{'op':>2}  "
        f"{'rtl_cycle':>10}  "
        f"{'cyc_cycle':>10}  "
        f"{'rtl_dur':>9}  "
        f"{'cyc_dur':>9}  "
        f"{'diff':>9}  "
        f"{'%diff':>9}"
    )
    print(
        "-"
        * (
            suite_w
            + pattern_w
            + 2
            + 10
            + 10
            + 9
            + 9
            + 9
            + 9
            + 18
        )
    )
    for rtl, cyc, diff in rows:
        print(
            f"{rtl.suite:<{suite_w}}  "
            f"{rtl.pattern:<{pattern_w}}  "
            f"{rtl.op:>2}  "
            f"{rtl.cycle:>10}  "
            f"{cyc.cycle:>10}  "
            f"{rtl.duration:>9}  "
            f"{cyc.duration:>9}  "
            f"{diff:>9}  "
            f"{format_pct(pct_diff(diff, rtl.duration)):>9}"
        )


def print_overall(comparisons: list[FileComparison]) -> None:
    all_rows = [row for comparison in comparisons for row in comparison.rows]
    score = score_rows(all_rows)
    print("")
    print("overall")
    print(f"{'files':<16}{len(comparisons):>12}")
    print(f"{'rows':<16}{score.count:>12}")
    print(f"{'mae':<16}{score.mae:>12.3f}")
    print(f"{'rmse':<16}{score.rmse:>12.3f}")
    print(f"{'max_abs_diff':<16}{score.max_abs:>12}")
    print(f"{'total_abs_diff':<16}{score.total_abs:>12}")


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    cyclotron_root = script_dir.parent
    default_logs = cyclotron_root.parent / "tuning_files" / "smem"

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "rtl",
        nargs="?",
        type=Path,
        default=default_logs,
        help="RTL .out file or directory. Default: ../tuning_files/smem",
    )
    parser.add_argument(
        "--sim-config",
        type=Path,
        default=cyclotron_root / "config.toml",
        help="Cyclotron sim config path",
    )
    parser.add_argument(
        "--run-timeout-seconds",
        type=int,
        default=120,
        help="timeout for each Cyclotron run",
    )
    args = parser.parse_args()

    rtl_path = args.rtl.resolve()
    if not rtl_path.exists():
        print(f"RTL path not found: {rtl_path}", file=sys.stderr)
        return 1

    log_paths = discover_logs(rtl_path)
    if rtl_path.is_dir():
        log_paths = [path for path in log_paths if path.name in USEFUL_LOGS]
    if not log_paths:
        print(f"no useful SMEM RTL logs found under {rtl_path}", file=sys.stderr)
        return 1

    comparisons: list[FileComparison] = []
    with tempfile.TemporaryDirectory(prefix="cyclotron_smem_compare_") as td:
        tmpdir = Path(td)
        for rtl_out in log_paths:
            comparison = compare_file(
                cyclotron_root,
                args.sim_config.resolve(),
                rtl_out,
                tmpdir,
                args.run_timeout_seconds,
            )
            comparisons.append(comparison)
            print_file_table(comparison)

    print_overall(comparisons)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
