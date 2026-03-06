#!/usr/bin/env python3

import argparse
import sqlite3
import struct
import sys
from dataclasses import dataclass
from typing import Dict, List, Sequence


@dataclass(frozen=True)
class ElementEvent:
    row_id: int
    store: bool
    element_addr: int
    value_bits: int


def parse_int(value: str) -> int:
    return int(value, 0)


def bf16_bits_to_float(bits: int) -> float:
    return struct.unpack(">f", struct.pack(">I", (bits & 0xFFFF) << 16))[0]


def format_bf16(bits: int) -> str:
    return f"{bf16_bits_to_float(bits):.8g}"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Print bf16 values from a Cyclotron sqlite dmem trace."
    )
    parser.add_argument("db", help="Path to sqlite trace database")
    parser.add_argument("address", type=parse_int, help="Start address, e.g. 0x80000000")
    parser.add_argument(
        "rows",
        type=int,
        help="Number of elements, or number of rows when cols is given",
    )
    parser.add_argument(
        "cols",
        type=int,
        nargs="?",
        help="Optional number of columns; when present, print a rows x cols matrix",
    )
    parser.add_argument(
        "--cols",
        type=int,
        dest="cols_flag",
        help="Compatibility alias for positional cols",
    )
    parser.add_argument(
        "--subrow",
        type=int,
        nargs="+",
        help="Matrix slice per row: END means [0, END), START END means [START, END)",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--initial",
        action="store_true",
        help="Print the first load seen for each element in the range",
    )
    mode.add_argument(
        "--final",
        action="store_true",
        help="Print the last store seen for each element in the range",
    )
    mode.add_argument(
        "--all",
        action="store_true",
        help="Print all reads and writes seen for each element in the range",
    )
    return parser


def parse_subrow(subrow_args: Sequence[int] | None, cols: int | None) -> tuple[int, int] | None:
    if subrow_args is None:
        return None
    if cols is None:
        raise SystemExit("--subrow requires matrix shape with cols")
    if len(subrow_args) == 1:
        start, end = 0, subrow_args[0]
    elif len(subrow_args) == 2:
        start, end = subrow_args
    else:
        raise SystemExit("--subrow accepts either END or START END")
    if start < 0 or end < 0:
        raise SystemExit("--subrow values must be non-negative")
    if start >= end:
        raise SystemExit("--subrow requires START < END")
    if end > cols:
        raise SystemExit("--subrow end cannot be larger than cols")
    return start, end


def requested_addresses(
    base_addr: int,
    rows: int,
    cols: int | None,
    subrow: tuple[int, int] | None,
) -> List[int]:
    if cols is None:
        return [base_addr + 2 * i for i in range(rows)]

    start_col = 0 if subrow is None else subrow[0]
    end_col = cols if subrow is None else subrow[1]
    addresses: List[int] = []
    for row in range(rows):
        row_base = base_addr + 2 * row * cols
        for col in range(start_col, end_col):
            addresses.append(row_base + 2 * col)
    return addresses


def fetch_events(
    conn: sqlite3.Connection,
    requested: Sequence[int],
) -> List[ElementEvent]:
    if not requested:
        return []

    requested_set = set(requested)
    min_addr = min(requested)
    max_addr = max(requested) + 2
    cursor = conn.execute(
        """
        SELECT id, store, address, size, data
        FROM dmem
        WHERE address < ? AND (address + size) > ?
        ORDER BY id
        """,
        (max_addr, min_addr),
    )

    events: List[ElementEvent] = []
    for row_id, store, address, size, data in cursor:
        size_bytes = int(size)
        if size_bytes == 1:
            if min_addr <= address < max_addr:
                raise ValueError(
                    f"byte access at row id {row_id} overlaps requested bf16 range; "
                    "cannot reconstruct a full bf16 value"
                )
            continue
        if size_bytes % 2 != 0:
            raise ValueError(f"unsupported access width at row id {row_id}: {size_bytes} bytes")

        chunks = size_bytes // 2
        for chunk in range(chunks):
            element_addr = int(address) + 2 * chunk
            if element_addr not in requested_set:
                continue
            value_bits = (int(data) >> (16 * chunk)) & 0xFFFF
            events.append(
                ElementEvent(
                    row_id=int(row_id),
                    store=bool(store),
                    element_addr=element_addr,
                    value_bits=value_bits,
                )
            )
    return events


def collect_mode_values(
    addresses: Sequence[int],
    events: Sequence[ElementEvent],
    mode: str,
) -> List[str]:
    per_addr: Dict[int, List[ElementEvent]] = {addr: [] for addr in addresses}
    for event in events:
        per_addr[event.element_addr].append(event)

    values: List[str] = []
    for addr in addresses:
        addr_events = per_addr[addr]
        if mode == "initial":
            loads = [event for event in addr_events if not event.store]
            values.append(format_bf16(loads[0].value_bits) if loads else "NA")
        elif mode == "final":
            stores = [event for event in addr_events if event.store]
            values.append(format_bf16(stores[-1].value_bits) if stores else "NA")
        else:
            if not addr_events:
                values.append("[]")
                continue
            rendered = ", ".join(
                f"{'W' if event.store else 'R'}({format_bf16(event.value_bits)})"
                for event in addr_events
            )
            values.append(f"[{rendered}]")
    return values


def format_output(
    values: Sequence[str],
    rows: int,
    cols: int | None,
    subrow: tuple[int, int] | None,
) -> str:
    if cols is None:
        return "[" + ", ".join(values) + "]"

    visible_cols = cols if subrow is None else subrow[1] - subrow[0]
    lines = ["["]
    for row in range(rows):
        start = row * visible_cols
        end = start + visible_cols
        lines.append("  [" + ", ".join(values[start:end]) + "]")
    lines.append("]")
    return "\n".join(lines)


def main() -> int:
    args = build_parser().parse_args()

    if args.rows <= 0:
        raise SystemExit("rows must be positive")
    if args.cols is not None and args.cols <= 0:
        raise SystemExit("cols must be positive")
    if args.cols_flag is not None and args.cols_flag <= 0:
        raise SystemExit("--cols must be positive")
    if args.cols is not None and args.cols_flag is not None:
        raise SystemExit("specify cols either positionally or with --cols, not both")

    rows = args.rows
    cols = args.cols if args.cols is not None else args.cols_flag
    subrow = parse_subrow(args.subrow, cols)
    base_addr = args.address
    mode = "final"
    if args.initial:
        mode = "initial"
    elif args.all:
        mode = "all"

    conn = sqlite3.connect(args.db)
    try:
        addresses = requested_addresses(base_addr, rows, cols, subrow)
        events = fetch_events(conn, addresses)
        values = collect_mode_values(addresses, events, mode)
    finally:
        conn.close()

    print(format_output(values, rows, cols, subrow))
    return 0


if __name__ == "__main__":
    sys.exit(main())
