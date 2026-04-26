#!/usr/bin/env python3
"""Unit tests for SMEM [STIM] parser support."""

from __future__ import annotations

import unittest
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import compare_smem_run  # noqa: E402
import tune_smem_primitives  # noqa: E402


class StimulusSchemaParserTest(unittest.TestCase):
    SAMPLE = """
[STIM] domain=smem suite=smem_muon_traffic phase=traffic pattern=strided(1,_1)@4_w op=w cycle=4100 duration=4100 reqs_per_lane=4096 active_lanes=16 issue_gap=0 max_outstanding=8 working_set=131072 wait_cycles=0 note=muon_traffic
[STIM] domain=smem suite=smem_muon_traffic phase=traffic pattern=strided(1,_1)@4_r op=r cycle=8201 duration=4101 reqs_per_lane=4096 active_lanes=16 issue_gap=0 max_outstanding=8 working_set=131072 wait_cycles=0 note=muon_traffic
[STIM] domain=smem suite=smem_queue_depth_probe phase=traffic pattern=strided(2,_8)@4_w op=w cycle=     32772 duration=     32772 reqs_per_lane=4096 active_lanes=16 issue_gap=0 max_outstanding=1 working_set=131072 wait_cycles=0 note=queue_probe_mo1
[STIM] domain=smem suite=smem_mixed_muon_gemmini phase=traffic pattern=mixed_partition_muon_w_gemmini_r_strided(1,_1)@4_w_plus_tiled(32,_32)@4_r op=wr cycle=40000 duration=7228 reqs_per_lane=2048 active_lanes=16 issue_gap=0 max_outstanding=12 working_set=131072 wait_cycles=0 note=mixed
[STIM] domain=gmem suite=gmem_hierarchy phase=traffic pattern=ignored op=r cycle=9999 duration=10 reqs_per_lane=1 active_lanes=1 issue_gap=0 max_outstanding=1 working_set=64 wait_cycles=0 note=na
"""

    def test_compare_script_parser_extracts_only_smem_traffic_rows(self) -> None:
        cps = compare_smem_run.parse_cyclotron_rows(self.SAMPLE, "sample.out")
        self.assertEqual(
            [cp.pattern for cp in cps],
            [
                "strided(1,_1)@4_w",
                "strided(1,_1)@4_r",
                "strided(2,_8)@4_w",
                "mixed_partition_muon_w_gemmini_r_strided(1,_1)@4_w_plus_tiled(32,_32)@4_r",
            ],
        )
        self.assertEqual([cp.cycle for cp in cps], [4100, 8201, 32772, 40000])

    def test_tuning_script_parser_extracts_only_smem_traffic_rows(self) -> None:
        cps = tune_smem_primitives.parse_rows(self.SAMPLE, "sample.out")
        self.assertEqual(len(cps), 4)
        self.assertEqual(cps[0].pattern, "strided(1,_1)@4_w")
        self.assertEqual(cps[1].cycle, 8201)
        self.assertEqual(cps[0].duration, 4100)

    def test_alignment_uses_shared_pattern_key(self) -> None:
        rtl = compare_smem_run.parse_cyclotron_rows(self.SAMPLE, "rtl.out")[:2]
        cyclotron = [
            tune_smem_primitives.StimRow(
                source="cyclotron",
                suite="smem_muon_traffic",
                pattern="strided(1,_1)@4_w",
                op="w",
                cycle=4200,
                duration=4200,
                reqs_per_lane=4096,
                active_lanes=16,
                issue_gap=0,
                max_outstanding=8,
                working_set=131072,
                note="cyclotron",
            ),
            tune_smem_primitives.StimRow(
                source="cyclotron",
                suite="smem_muon_traffic",
                pattern="strided(1,_1)@4_r",
                op="r",
                cycle=8300,
                duration=4100,
                reqs_per_lane=4096,
                active_lanes=16,
                issue_gap=0,
                max_outstanding=8,
                working_set=131072,
                note="cyclotron",
            ),
        ]
        rows = tune_smem_primitives.align(rtl, cyclotron)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0][0].pattern, "strided(1,_1)@4_w")


if __name__ == "__main__":
    unittest.main()
