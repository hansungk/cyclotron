#!/usr/bin/env python3
"""Tune the primitive SMEM pipeline against Radiance RTL stimulus logs.

The script parses the new `[STIM]` schema, generates temporary traffic frontend
TOMLs, runs Cyclotron in `traffic_smem` mode, and tunes one shared
`config/timing/smem.toml` across all useful SMEM logs.
"""

from __future__ import annotations

import argparse
import math
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


STIM_RE = re.compile(r"\[STIM\]\s+(.*)$")
KV_RE = re.compile(r"(\w+)=\s*([^\s]+)")
USEFUL_LOGS = {
    "smem_muon_traffic.out",
    "smem_queue_depth.out",
    "smem_throughput.out",
    "smem_gemmini_facing_probe.out",
    "smem_mixed_muon_gemmini.out",
}

INT_KEYS = [
    "base_overhead",
    "read_extra",
    "prealign_latency",
    "crossbar_latency",
    "port_latency_read",
    "port_latency_write",
]
POSITIVE_KEYS = [
    "prealign_buf_depth",
    "port_queue_depth",
    "completion_queue_depth",
    "max_outstanding",
    "prealign_bytes_per_cycle",
    "crossbar_bytes_per_cycle",
    "bank_read_bytes_per_cycle",
    "bank_write_bytes_per_cycle",
    "tail_bytes_per_cycle",
]
TUNED_POSITIVE_KEYS: list[str] = [
    "port_queue_depth",
    "completion_queue_depth",
]
FIXED_PARAMS: dict[str, int | str] = {
    # TapeoutSmemConfig in radiance/chipyard/RadianceConfigs.scala.
    "address_map": "rtl_contiguous_banks",
    "serialization": "core_serialized",
    "mem_type": "two_port",
    "prealign_buf_depth": 2,
    "max_outstanding": 512,
    # Physical word/subbank width. Wide Gemmini-facing requests are decomposed
    # into word-sized timed-server bank-port operations in the Cyclotron model.
    "prealign_bytes_per_cycle": 1,
    "crossbar_bytes_per_cycle": 4,
    "bank_read_bytes_per_cycle": 4,
    "bank_write_bytes_per_cycle": 4,
    "tail_bytes_per_cycle": 1,
}
STRING_CHOICES = {
    "address_map": ["rtl_contiguous_banks", "subbank_then_bank", "bank_then_subbank"],
    "serialization": ["core_serialized", "not_serialized"],
    "mem_type": ["two_port", "two_read_one_write"],
}


@dataclass(frozen=True)
class StimRow:
    source: str
    suite: str
    pattern: str
    op: str
    cycle: int
    duration: int
    reqs_per_lane: int
    active_lanes: int
    issue_gap: int
    max_outstanding: int
    working_set: int
    note: str


@dataclass(frozen=True)
class Score:
    mae: float
    rmse: float
    max_abs: int
    total_abs: int
    count: int


def parse_rows(text: str, source: str) -> list[StimRow]:
    rows: list[StimRow] = []
    for line in text.splitlines():
        match = STIM_RE.search(line)
        if not match:
            continue
        kv = dict(KV_RE.findall(match.group(1)))
        if kv.get("domain") != "smem" or kv.get("phase") != "traffic":
            continue
        rows.append(
            StimRow(
                source=source,
                suite=kv["suite"],
                pattern=kv["pattern"],
                op=kv["op"],
                cycle=int(kv["cycle"]),
                duration=int(kv["duration"]),
                reqs_per_lane=int(kv["reqs_per_lane"]),
                active_lanes=int(kv["active_lanes"]),
                issue_gap=int(kv["issue_gap"]),
                max_outstanding=int(kv["max_outstanding"]),
                working_set=int(kv["working_set"]),
                note=kv.get("note", "na"),
            )
        )
    return rows


def parse_out(path: Path) -> list[StimRow]:
    return parse_rows(path.read_text(errors="replace"), path.name)


def discover_logs(path: Path) -> list[Path]:
    if path.is_file():
        return [path]
    return sorted(p for p in path.glob("*.out") if p.name in USEFUL_LOGS)


def parse_pattern(row: StimRow) -> dict[str, str | int | bool]:
    pattern = row.pattern
    req_bytes = 4
    transpose = False

    m = re.search(r"strided\((\d+),_(\d+)\)@(\d+)", pattern)
    if m:
        warp_stride, lane_stride, req_bytes_s = m.groups()
        return {
            "kind": "strided",
            "req_bytes": int(req_bytes_s),
            "warp_stride": int(warp_stride),
            "lane_stride": int(lane_stride),
        }

    m = re.search(r"tiled\((\d+),_(\d+)\)@(\d+)(\.T)?", pattern)
    if m:
        tile_m, tile_n, req_bytes_s, t = m.groups()
        return {
            "kind": "tiled",
            "req_bytes": int(req_bytes_s),
            "tile_m": int(tile_m),
            "tile_n": int(tile_n),
            "transpose": bool(t),
        }

    m = re.search(r"swizzled\((\d+)\)@(\d+)(\.T)?", pattern)
    if m:
        tile_size, req_bytes_s, t = m.groups()
        return {
            "kind": "swizzled",
            "req_bytes": int(req_bytes_s),
            "tile_size": int(tile_size),
            "transpose": bool(t),
        }

    m = re.search(r"random\((\d+)\)", pattern)
    if m:
        seed = int(m.group(1))
        req_match = re.search(r"@(\d+)", pattern)
        if req_match:
            req_bytes = int(req_match.group(1))
        return {
            "kind": "random",
            "req_bytes": req_bytes,
            "random_min": 0,
            "random_max": max(1, row.working_set // max(1, req_bytes)),
            "seed": seed,
        }

    raise ValueError(f"unsupported SMEM pattern syntax: {pattern}")


def base_offset(row: StimRow) -> int:
    if "base0" in row.note:
        return 0
    m = re.search(r"base(\d+)k", row.note)
    if m:
        return int(m.group(1)) << 10
    return 64 << 10 if row.op == "r" else 0


def toml_value(value: str | int | bool) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    return f'"{value}"'


def write_traffic_config(path: Path, rows: list[StimRow]) -> None:
    lines: list[str] = [
        "[traffic]",
        "enabled = true",
        "lockstep_patterns = true",
        "reqs_per_pattern = 4096",
        "num_lanes = 16",
        "",
        "[traffic.address]",
        "cluster_id = 0",
        "smem_base = 0x40000000",
        "smem_size_bytes = 131072",
        "",
        "[traffic.issue]",
        "max_inflight_per_lane = 16",
        "retry_backoff_min = 1",
        "",
        "[traffic.logging]",
        "print_traffic_lines = true",
        "",
    ]
    for row in rows:
        fields = parse_pattern(row)
        fields.update(
            {
                "suite": row.suite,
                "name": row.pattern,
                "op": "read" if row.op == "r" else "write",
                "reqs_per_pattern": row.reqs_per_lane,
                "active_lanes": row.active_lanes,
                "max_inflight_per_lane": row.max_outstanding,
                "issue_gap": row.issue_gap,
                "base_offset_bytes": base_offset(row),
                "within_bytes": row.working_set,
            }
        )
        lines.append("[[traffic.patterns]]")
        for key, value in fields.items():
            lines.append(f"{key} = {toml_value(value)}")
        lines.append("")
    path.write_text("\n".join(lines))


def parse_cyclotron_rows(text: str, source: str) -> list[StimRow]:
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
        raise RuntimeError("Cyclotron emitted no SMEM traffic rows")
    return rows


def align(rtl: list[StimRow], cyc: list[StimRow]) -> list[tuple[StimRow, StimRow, int]]:
    if len(rtl) != len(cyc):
        raise RuntimeError(f"row count mismatch: rtl={len(rtl)} cyclotron={len(cyc)}")
    cyc_by_key = {(row.suite, row.pattern, row.op): row for row in cyc}
    if len(cyc_by_key) != len(cyc):
        raise RuntimeError("Cyclotron emitted duplicate (suite, pattern, op) rows")
    out: list[tuple[StimRow, StimRow, int]] = []
    for r in rtl:
        key = (r.suite, r.pattern, r.op)
        c = cyc_by_key.get(key)
        if c is None:
            raise RuntimeError(f"Cyclotron missing row {key}")
        out.append((r, c, c.duration - r.duration))
    return out


def score_rows(aligned: Iterable[tuple[StimRow, StimRow, int]]) -> Score:
    diffs = [diff for _r, _c, diff in aligned]
    if not diffs:
        return Score(0.0, 0.0, 0, 0, 0)
    abs_diffs = [abs(x) for x in diffs]
    return Score(
        mae=sum(abs_diffs) / len(abs_diffs),
        rmse=math.sqrt(sum(x * x for x in diffs) / len(diffs)),
        max_abs=max(abs_diffs),
        total_abs=sum(abs_diffs),
        count=len(diffs),
    )


def read_param_text(path: Path) -> str:
    return path.read_text()


def extract_int(text: str, key: str) -> int:
    m = re.search(rf"^(\s*{re.escape(key)}\s*=\s*)(\d+)\b", text, re.MULTILINE)
    if not m:
        raise RuntimeError(f"missing integer SMEM key: {key}")
    return int(m.group(2))


def extract_str(text: str, key: str) -> str:
    m = re.search(rf'^\s*{re.escape(key)}\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not m:
        raise RuntimeError(f"missing string SMEM key: {key}")
    return m.group(1)


def replace_int(text: str, key: str, value: int) -> str:
    pattern = re.compile(rf"^(\s*{re.escape(key)}\s*=\s*)(\d+)(.*)$", re.MULTILINE)
    text, count = pattern.subn(rf"\g<1>{int(value)}\g<3>", text, count=1)
    if count != 1:
        raise RuntimeError(f"failed to replace {key}")
    return text


def replace_str(text: str, key: str, value: str) -> str:
    pattern = re.compile(rf'^(\s*{re.escape(key)}\s*=\s*")([^"]+)(".*)$', re.MULTILINE)
    text, count = pattern.subn(rf"\g<1>{value}\g<3>", text, count=1)
    if count != 1:
        raise RuntimeError(f"failed to replace {key}")
    return text


def load_params(path: Path) -> dict[str, int | str]:
    text = read_param_text(path)
    params: dict[str, int | str] = {}
    for key in INT_KEYS + POSITIVE_KEYS:
        params[key] = extract_int(text, key)
    for key in STRING_CHOICES:
        params[key] = extract_str(text, key)
    return params


def write_params(path: Path, params: dict[str, int | str]) -> None:
    text = read_param_text(path)
    for key in INT_KEYS + POSITIVE_KEYS:
        text = replace_int(text, key, int(params[key]))
    for key in STRING_CHOICES:
        text = replace_str(text, key, str(params[key]))
    path.write_text(text)


def apply_fixed_params(params: dict[str, int | str]) -> dict[str, int | str]:
    out = dict(params)
    out.update(FIXED_PARAMS)
    return out


def evaluate(
    cyclotron_root: Path,
    sim_config: Path,
    smem_toml: Path,
    workloads: list[tuple[Path, list[StimRow]]],
    params: dict[str, int | str],
    tmpdir: Path,
    run_timeout_seconds: int,
) -> tuple[Score, dict[str, list[tuple[StimRow, StimRow, int]]]]:
    write_params(smem_toml, params)
    all_aligned: list[tuple[StimRow, StimRow, int]] = []
    by_file: dict[str, list[tuple[StimRow, StimRow, int]]] = {}
    for idx, (source, rtl_rows) in enumerate(workloads):
        traffic_config = tmpdir / f"smem_workload_{idx}_{source.stem}.toml"
        write_traffic_config(traffic_config, rtl_rows)
        cyc_rows = run_cyclotron(cyclotron_root, sim_config, traffic_config, run_timeout_seconds)
        aligned = align(rtl_rows, cyc_rows)
        by_file[source.name] = aligned
        all_aligned.extend(aligned)
    return score_rows(all_aligned), by_file


def score_key(score: Score, params: dict[str, int | str], start: dict[str, int | str]) -> tuple:
    changed = sum(
        1 for key, value in params.items() if value != start.get(key)
    )
    return (score.mae, score.rmse, score.max_abs, score.total_abs, changed)


def print_score(label: str, params: dict[str, int | str], score: Score) -> None:
    knobs = ", ".join(
        f"{k}={params[k]}" for k in ["address_map", "serialization", "mem_type"] + INT_KEYS
    )
    print(
        f"{label} {knobs} | "
        f"count={score.count} mae={score.mae:.3f} rmse={score.rmse:.3f} "
        f"max={score.max_abs} total_abs={score.total_abs}"
    )


def format_report(
    best_params: dict[str, int | str],
    best_score: Score,
    by_file: dict[str, list[tuple[StimRow, StimRow, int]]],
) -> str:
    lines = ["smem_primitive_pipeline_tuning_report", ""]
    lines.append("fixed_structural_params_source = radiance/chipyard/RadianceConfigs.scala:TapeoutSmemConfig")
    lines.append("")
    lines.append("best_params")
    for key in ["address_map", "serialization", "mem_type"] + INT_KEYS + POSITIVE_KEYS:
        lines.append(f"{key} = {best_params[key]}")
    lines.append("")
    lines.append(
        f"overall count={best_score.count} mae={best_score.mae:.3f} "
        f"rmse={best_score.rmse:.3f} max_abs={best_score.max_abs} "
        f"total_abs={best_score.total_abs}"
    )
    for name, rows in by_file.items():
        score = score_rows(rows)
        lines.append("")
        lines.append(
            f"[{name}] count={score.count} mae={score.mae:.3f} "
            f"rmse={score.rmse:.3f} max_abs={score.max_abs} total_abs={score.total_abs}"
        )
        lines.append(
            f"{'suite':<30} {'pattern':<58} {'rtl_dur':>10} {'cyc_dur':>10} {'diff':>9}"
        )
        lines.append("-" * 122)
        for rtl, cyc, diff in rows:
            lines.append(
                f"{rtl.suite:<30} {rtl.pattern:<58} "
                f"{rtl.duration:>10} {cyc.duration:>10} {diff:>9}"
            )
    return "\n".join(lines) + "\n"


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    cyclotron_root = script_dir.parent
    default_logs = cyclotron_root.parent / "tuning_files" / "smem"

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rtl", type=Path, default=default_logs, help="RTL .out file or directory")
    parser.add_argument("--sim-config", type=Path, default=cyclotron_root / "config.toml")
    parser.add_argument("--smem-toml", type=Path, default=cyclotron_root / "config/timing/smem.toml")
    parser.add_argument(
        "--report-txt",
        type=Path,
        default=cyclotron_root / "logs/traffic/smem_primitive_pipeline_tuning.txt",
    )
    parser.add_argument("--max-iterations", type=int, default=2)
    parser.add_argument("--max-evals", type=int, default=80)
    parser.add_argument("--skip-string-search", action="store_true")
    parser.add_argument(
        "--include-fully-serialized",
        action="store_true",
        help="also try the slow fully_serialized structural option",
    )
    parser.add_argument("--run-timeout-seconds", type=int, default=120)
    args = parser.parse_args()

    string_choices = {key: list(values) for key, values in STRING_CHOICES.items()}
    if args.include_fully_serialized:
        string_choices["serialization"].insert(1, "fully_serialized")

    log_paths = discover_logs(args.rtl.resolve())
    if not log_paths:
        print(f"no useful SMEM RTL logs found under {args.rtl}", file=sys.stderr)
        return 1
    workloads = [(path, parse_out(path)) for path in log_paths]
    workloads = [(path, rows) for path, rows in workloads if rows]
    print("using RTL logs:")
    for path, rows in workloads:
        print(f"  {path.name}: {len(rows)} traffic rows")

    smem_toml = args.smem_toml.resolve()
    original = smem_toml.read_text()
    start = load_params(smem_toml)
    best_params = apply_fixed_params(start)

    succeeded = False
    try:
        with tempfile.TemporaryDirectory(prefix="cyclotron_smem_tune_") as td:
            tmpdir = Path(td)
            evals = 0
            best_score, best_rows = evaluate(
                cyclotron_root,
                args.sim_config.resolve(),
                smem_toml,
                workloads,
                best_params,
                tmpdir,
                args.run_timeout_seconds,
            )
            evals += 1
            print_score("[baseline]", best_params, best_score)

            if not args.skip_string_search:
                for key, choices in string_choices.items():
                    for choice in choices:
                        if evals >= args.max_evals:
                            break
                        cand = dict(best_params)
                        cand[key] = choice
                        cand = apply_fixed_params(cand)
                        score, rows = evaluate(
                            cyclotron_root,
                            args.sim_config.resolve(),
                            smem_toml,
                            workloads,
                            cand,
                            tmpdir,
                            args.run_timeout_seconds,
                        )
                        evals += 1
                        print_score(f"[scan {key}]", cand, score)
                        if score_key(score, cand, start) < score_key(best_score, best_params, start):
                            best_params, best_score, best_rows = cand, score, rows
                            print_score("[improved]", best_params, best_score)
                    if evals >= args.max_evals:
                        break

            for radius in [8, 4, 2, 1]:
                if args.max_iterations <= 0 or evals >= args.max_evals:
                    break
                for _ in range(args.max_iterations):
                    improved = False
                    for key in INT_KEYS + TUNED_POSITIVE_KEYS:
                        if evals >= args.max_evals:
                            break
                        deltas = [-radius, radius]
                        for delta in deltas:
                            if evals >= args.max_evals:
                                break
                            cand = dict(best_params)
                            floor = 1 if key in POSITIVE_KEYS else 0
                            cand[key] = max(floor, int(cand[key]) + delta)
                            score, rows = evaluate(
                                cyclotron_root,
                                args.sim_config.resolve(),
                                smem_toml,
                                workloads,
                                cand,
                                tmpdir,
                                args.run_timeout_seconds,
                            )
                            evals += 1
                            print_score(f"[r={radius} {key}{delta:+}]", cand, score)
                            if score_key(score, cand, start) < score_key(best_score, best_params, start):
                                best_params, best_score, best_rows = cand, score, rows
                                improved = True
                                print_score("[improved]", best_params, best_score)
                    if not improved:
                        break

            write_params(smem_toml, best_params)
            args.report_txt.parent.mkdir(parents=True, exist_ok=True)
            args.report_txt.write_text(format_report(best_params, best_score, best_rows))
            print_score("[best]", best_params, best_score)
            print(f"[report] wrote {args.report_txt}")
            succeeded = True
            return 0
    finally:
        if not succeeded:
            smem_toml.write_text(original)


if __name__ == "__main__":
    raise SystemExit(main())
