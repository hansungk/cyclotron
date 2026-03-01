#!/usr/bin/env python3
"""
SMEM parity validator.

Behavior:
1) Generate traffic TOML from a reference `.out`.
2) Run Cyclotron STF mode.
3) Enforce strict in-order pattern matching.
4) Print line-by-line absolute cycle error and accumulated errors.
"""

from __future__ import annotations

import argparse
import re
import shlex
import subprocess
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from generate_smem_traffic_toml import parse_int_auto

TRAFFIC_LINE_RE = re.compile(
    r"\[TRAFFIC\]\s+core\s+(?P<core>\d+)\s+(?P<name>.+?)\s+finished at time\s+(?P<cycle>\d+)\s*$"
)


@dataclass
class TrafficLine:
    pattern_name: str
    cycle: int


def parse_traffic_lines(path: Path, core: int) -> list[TrafficLine]:
    rows: list[TrafficLine] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        match = TRAFFIC_LINE_RE.search(raw)
        if not match:
            continue
        if int(match.group("core")) != core:
            continue
        rows.append(
            TrafficLine(
                pattern_name=match.group("name").strip(),
                cycle=int(match.group("cycle")),
            )
        )
    return rows


def traffic_type(pattern_name: str) -> str:
    if "(" in pattern_name:
        return pattern_name.split("(", 1)[0]
    return pattern_name.rsplit("_", 1)[0]


def append_traffic_file_to_config(base_text: str, traffic_file_rel: str) -> str:
    if "[traffic]" in base_text:
        raise RuntimeError(
            "base config already contains a [traffic] table; provide a config without [traffic]"
        )
    text = base_text
    if not text.endswith("\n"):
        text += "\n"
    text += "\n[traffic]\n"
    text += f'file = "{traffic_file_rel}"\n'
    return text


def override_sim_timeout(base_text: str, timeout_cycles: int) -> str:
    lines = base_text.splitlines()
    out: list[str] = []
    in_sim = False
    replaced = False

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_sim = stripped == "[sim]"
            out.append(line)
            continue
        if in_sim and stripped.startswith("timeout"):
            out.append(f"timeout = {timeout_cycles}")
            replaced = True
            continue
        out.append(line)

    if not replaced:
        rebuilt: list[str] = []
        inserted = False
        in_sim = False
        for line in out:
            stripped = line.strip()
            if stripped == "[sim]":
                in_sim = True
                rebuilt.append(line)
                continue
            if in_sim and stripped.startswith("[") and stripped.endswith("]") and not inserted:
                rebuilt.append(f"timeout = {timeout_cycles}")
                inserted = True
                in_sim = False
            rebuilt.append(line)
        if in_sim and not inserted:
            rebuilt.append(f"timeout = {timeout_cycles}")
        out = rebuilt

    return "\n".join(out) + ("\n" if base_text.endswith("\n") else "")


def run_command(
    cmd: list[str], cwd: Path, timeout_s: int, log_path: Path | None = None
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout_s,
        check=False,
    )
    if log_path is not None:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.write_text(proc.stdout, encoding="utf-8")
    return proc


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-out", required=True)
    parser.add_argument(
        "--cyclotron-root",
        default=str(Path(__file__).resolve().parents[1]),
        help="Path to Cyclotron repo root",
    )
    parser.add_argument("--base-config", default="config.toml")
    parser.add_argument("--traffic-config", default="config/traffic/smem_radiance.toml")
    parser.add_argument("--cyclotron-cmd", default="cargo run --release --")
    parser.add_argument("--core", type=int, default=0)
    parser.add_argument("--num-lanes", type=int, default=16)
    parser.add_argument("--num-warps", type=int, default=16)
    parser.add_argument("--reqs-per-pattern", type=int, default=4096)
    parser.add_argument("--sim-timeout", type=int, default=50_000_000)
    parser.add_argument("--smem-base", type=parse_int_auto, default=0x40000000)
    parser.add_argument("--smem-size-bytes", type=int, default=128 << 10)
    parser.add_argument("--timeout-s", type=int, default=600)
    parser.add_argument(
        "--work-dir",
        help="Optional output directory. Default: logs/synethetic_traffic/<timestamp>",
    )
    args = parser.parse_args()

    cyclotron_root = Path(args.cyclotron_root).resolve()
    reference_out = Path(args.reference_out).resolve()
    traffic_cfg_path = (cyclotron_root / args.traffic_config).resolve()
    base_config_path = (cyclotron_root / args.base_config).resolve()

    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    work_dir = (
        Path(args.work_dir).resolve()
        if args.work_dir
        else (cyclotron_root / "logs" / "synethetic_traffic" / timestamp)
    )
    work_dir.mkdir(parents=True, exist_ok=True)

    gen_cmd = [
        "python3",
        str((cyclotron_root / "scripts" / "generate_smem_traffic_toml.py").resolve()),
        "--reference-out",
        str(reference_out),
        "--output",
        str(traffic_cfg_path),
        "--core",
        str(args.core),
        "--num-lanes",
        str(args.num_lanes),
        "--reqs-per-pattern",
        str(args.reqs_per_pattern),
        "--smem-base",
        hex(args.smem_base),
        "--smem-size-bytes",
        str(args.smem_size_bytes),
    ]
    gen_proc = run_command(gen_cmd, cwd=cyclotron_root, timeout_s=60)
    if gen_proc.returncode != 0:
        print(gen_proc.stdout)
        raise SystemExit("traffic config generation failed")

    base_text = base_config_path.read_text(encoding="utf-8")
    base_text = override_sim_timeout(base_text, args.sim_timeout)
    run_config = append_traffic_file_to_config(base_text, args.traffic_config)
    run_config_path = base_config_path.parent / f".traffic_run_{timestamp}.toml"
    run_config_path.write_text(run_config, encoding="utf-8")

    cyclotron_cmd = shlex.split(args.cyclotron_cmd)
    cyclotron_cmd.extend(
        [
            str(run_config_path),
            "--timing",
            "--frontend-mode",
            "traffic_smem",
            "--num-warps",
            str(args.num_warps),
            "--num-lanes",
            str(args.num_lanes),
        ]
    )
    candidate_log = work_dir / "cyclotron_traffic.out"
    run_proc = run_command(
        cyclotron_cmd, cwd=cyclotron_root, timeout_s=args.timeout_s, log_path=candidate_log
    )
    if run_proc.returncode != 0:
        raise SystemExit(f"cyclotron run failed, see {candidate_log}")

    ref = parse_traffic_lines(reference_out, args.core)
    cand = parse_traffic_lines(candidate_log, args.core)
    if len(ref) != len(cand):
        raise SystemExit(
            f"line count mismatch: reference={len(ref)} candidate={len(cand)} "
            f"(see {candidate_log})"
        )

    print(
        "idx | traffic_type | pattern | RTL | cyclotron | error | accum_abs_err | accum_type_abs_err"
    )
    print("-" * 130)

    accum_abs_err = 0
    accum_by_type: dict[str, int] = defaultdict(int)
    for idx, (r, c) in enumerate(zip(ref, cand)):
        if r.pattern_name != c.pattern_name:
            raise SystemExit(
                f"order mismatch at idx {idx}: reference='{r.pattern_name}' candidate='{c.pattern_name}'"
            )

        error = c.cycle - r.cycle
        abs_error = abs(error)
        accum_abs_err += abs_error
        ttype = traffic_type(r.pattern_name)
        accum_by_type[ttype] += abs_error

        print(
            f"{idx:>3} | {ttype:<12} | {r.pattern_name:<24} | {r.cycle:>9} | {c.cycle:>10} | "
            f"{error:>7} | {accum_abs_err:>13} | {accum_by_type[ttype]:>18}"
        )

    print(f"candidate log: {candidate_log}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
