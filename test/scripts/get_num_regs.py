#!/usr/bin/env python3
import argparse
import re
import subprocess
import sys
from pathlib import Path

# Muon opcode values (9-bit)
LOAD = 0b000000011
CUSTOM0 = 0b000001011
MISC_MEM = 0b000001111
OP_IMM = 0b000010011
AUIPC = 0b000010111
STORE = 0b000100011
CUSTOM1 = 0b000101011
OP = 0b000110011
LUI = 0b000110111
MADD = 0b001000011
MSUB = 0b001000111
NM_SUB = 0b001001011
NM_ADD = 0b001001111
OP_FP = 0b001010011
CUSTOM2 = 0b001011011
BRANCH = 0b001100011
JALR = 0b001100111
JAL = 0b001101111
SYSTEM = 0b001110011
CUSTOM3 = 0b001111011

RTYPE_OPS = {
    CUSTOM0,
    CUSTOM1,
    CUSTOM2,
    CUSTOM3,
    OP,
    OP_FP,
    MADD,
    MSUB,
    NM_SUB,
    NM_ADD,
}


SECTION_RE = re.compile(
    r"^\s*\[\s*\d+\]\s+"          # [Nr]
    r"(?P<name>\S+)\s+"             # Name
    r"(?P<typ>\S+)\s+"              # Type
    r"(?P<addr>[0-9A-Fa-f]+)\s+"      # Address
    r"(?P<off>[0-9A-Fa-f]+)\s+"      # Off
    r"(?P<size>[0-9A-Fa-f]+)\s+"     # Size
    r"[0-9A-Fa-f]+\s+"               # ES
    r"(?P<flags>[A-Z]*)\s+"          # Flg
)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=(
            "Count unique architectural rd registers that can allocate rename-table "
            "entries (Rename.scala assigning path)."
        )
    )
    p.add_argument("elf", type=Path, help="Path to ELF file")
    p.add_argument(
        "--exclude-x0",
        action="store_true",
        help="Exclude x0 from the final count (default includes preallocated x0)",
    )
    p.add_argument(
        "--num-arch-regs",
        type=int,
        default=128,
        help=(
            "Architectural register count used by rename table indexing "
            "(default: 128, matching MuonCore numArchRegs)"
        ),
    )
    p.add_argument(
        "--sections",
        nargs="+",
        default=None,
        help="Explicit section names to analyze (overrides auto selection)",
    )
    return p.parse_args()


def readelf_sections(elf: Path):
    proc = subprocess.run(
        ["readelf", "-W", "-S", str(elf)],
        check=True,
        capture_output=True,
        text=True,
    )
    sections = []
    for line in proc.stdout.splitlines():
        m = SECTION_RE.match(line)
        if not m:
            continue
        sections.append(
            {
                "name": m.group("name"),
                "type": m.group("typ"),
                "addr": int(m.group("addr"), 16),
                "off": int(m.group("off"), 16),
                "size": int(m.group("size"), 16),
                "flags": m.group("flags"),
            }
        )
    return sections


def select_sections(sections, explicit):
    if explicit:
        wanted = set(explicit)
        sel = [s for s in sections if s["name"] in wanted and s["size"] > 0]
        missing = sorted(wanted - {s["name"] for s in sel})
        if missing:
            raise ValueError(f"Requested section(s) not found or empty: {', '.join(missing)}")
        return sel

    names = {s["name"] for s in sections}
    seg_names = [n for n in (".rv32.seg0", ".rv32.seg2") if n in names]
    if seg_names:
        return [s for s in sections if s["name"] in set(seg_names) and s["size"] > 0]

    # Fallback: executable PROGBITS sections.
    return [
        s
        for s in sections
        if s["type"] == "PROGBITS" and s["size"] > 0 and "X" in s["flags"]
    ]


def is_rtype(op: int) -> bool:
    return op in RTYPE_OPS


def is_stype(op: int) -> bool:
    return op == STORE


def is_btype(op: int) -> bool:
    return op == BRANCH


def is_ujtype(op: int) -> bool:
    return op in {LUI, AUIPC, JAL}


def is_split(op: int, f3: int) -> bool:
    return op == CUSTOM0 and f3 == 0b010


def is_pred(op: int, f3: int) -> bool:
    return op == CUSTOM0 and f3 == 0b101


def is_fpex(op: int, f7: int) -> bool:
    # Decode.scala: op.endsWith("1111011") && f7 == "0101110"
    return (op & 0x7F) == 0x7B and f7 == 0b0101110


def is_fcvt_hack(op: int, f7: int) -> bool:
    # Decode.scala: (op == OP_FP && f7 == "?10?0??")
    return op == OP_FP and ((f7 >> 4) & 0b11) == 0b10 and ((f7 >> 2) & 0b1) == 0


def has_rd(op: int, f3: int) -> bool:
    # Must match Decode.scala HasRd exactly; rename allocation uses this bit.
    return not (
        is_btype(op)
        or is_stype(op)
        or is_split(op, f3)
        or is_pred(op, f3)
    )


def decode_rename_rd(inst: int):
    op = inst & 0x1FF
    rd = (inst >> 9) & 0xFF
    f3 = (inst >> 17) & 0x7
    if has_rd(op, f3):
        return rd
    return None


SPECIAL_CUSTOM0 = {
    0b000: "vx_tmc",
    0b001: "vx_wspawn",
    0b011: "vx_join",
    0b100: "vx_bar",
}


def detect_nonzero_rd_special_custom0(inst: int):
    op = inst & 0x1FF
    f3 = (inst >> 17) & 0x7
    rd = (inst >> 9) & 0xFF
    if op == CUSTOM0 and f3 in SPECIAL_CUSTOM0 and rd != 0:
        return SPECIAL_CUSTOM0[f3], rd
    return None


def main() -> int:
    args = parse_args()
    elf = args.elf
    if args.num_arch_regs <= 0:
        print("error: --num-arch-regs must be > 0", file=sys.stderr)
        return 2
    # Rename.scala indexes rd as decoded.rd.asTypeOf(aRegT), i.e. low bits only.
    # For non-power-of-two counts this still matches Chisel width truncation.
    arch_mask = (1 << ((args.num_arch_regs - 1).bit_length())) - 1

    if not elf.is_file():
        print(f"error: ELF not found: {elf}", file=sys.stderr)
        return 2

    try:
        sections = readelf_sections(elf)
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"error: failed to inspect ELF sections: {e}", file=sys.stderr)
        return 2

    try:
        selected = select_sections(sections, args.sections)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    if not selected:
        print("error: no suitable sections selected", file=sys.stderr)
        return 2

    data = elf.read_bytes()
    renamed_rd = set()
    offenders = []

    for sec in selected:
        sec_name = sec["name"]
        sec_addr = sec["addr"]
        off = sec["off"]
        size = sec["size"]
        if off + size > len(data):
            print(
                f"error: section {sec['name']} extends beyond file size",
                file=sys.stderr,
            )
            return 2
        blob = data[off : off + size]
        n_words = len(blob) // 8
        for i in range(n_words):
            inst = int.from_bytes(blob[i * 8 : (i + 1) * 8], byteorder="little", signed=False)
            special = detect_nonzero_rd_special_custom0(inst)
            if special is not None:
                opname, rd = special
                offenders.append((opname, rd, sec_name, sec_addr + i * 8))
            rd = decode_rename_rd(inst)
            # Rename.scala keeps x0 pre-assigned, so it never creates a new entry.
            if rd is not None:
                mapped_rd = rd & arch_mask
                if mapped_rd != 0:
                    renamed_rd.add(mapped_rd)

    if offenders:
        print(
            "ERROR: INVALID ELF - NONZERO RD ON vx_tmc/vx_wspawn/vx_join/vx_bar",
            file=sys.stderr,
        )
        for opname, rd, sec_name, pc in offenders[:32]:
            print(
                f"  {opname}: rd={rd} at {sec_name} pc=0x{pc:x}",
                file=sys.stderr,
            )
        if len(offenders) > 32:
            print(f"  ... and {len(offenders) - 32} more", file=sys.stderr)
        return 2

    # Rename.scala keeps x0 pre-assigned, so include it by default.
    total = len(renamed_rd) + (0 if args.exclude_x0 else 1)
    print(total)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
