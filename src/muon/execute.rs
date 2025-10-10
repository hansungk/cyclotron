use std::sync::{Arc, RwLock};
use std::fmt::Debug;
use num_derive::FromPrimitive;
use num_traits::ToPrimitive;
pub use num_traits::WrappingAdd;
use crate::base::mem::HasMemory;
use crate::muon::decode::{sign_ext, IssuedInst, InstBufEntry, RegFile};
use crate::sim::toy_mem::ToyMemory;
use log::debug;
use crate::utils::BitSlice;
use phf::phf_map;
use crate::muon::csr::CSRFile;
use crate::muon::scheduler::Scheduler;
use crate::muon::warp::Writeback;
use crate::neutrino::neutrino::Neutrino;

#[derive(Debug, Clone)]
pub struct Opcode;
impl Opcode {
    pub const LOAD: u16 = 0b0000011u16;
    pub const LOAD_FP: u16 = 0b0000111u16;
    pub const CUSTOM0: u16 = 0b0001011u16;
    pub const MISC_MEM: u16 = 0b0001111u16;
    pub const OP_IMM: u16 = 0b0010011u16;
    pub const AUIPC: u16 = 0b0010111u16;
    //  pub const OpImm32 : u16 = 0b0011011u16;
    pub const STORE: u16 = 0b0100011u16;
    pub const STORE_FP: u16 = 0b0100111u16;
    pub const CUSTOM1: u16 = 0b0101011u16;
    //  pub const Amo     : u16 = 0b0101111u16;
    pub const OP: u16 = 0b0110011u16;
    pub const LUI: u16 = 0b0110111u16;
    pub const OP32: u16 = 0b0111011u16;
    pub const MADD: u16 = 0b1000011u16;
    pub const MSUB: u16 = 0b1000111u16;
    pub const NM_SUB: u16 = 0b1001011u16;
    pub const NM_ADD: u16 = 0b1001111u16;
    pub const OP_FP: u16 = 0b1010011u16;
    //  pub const OpV     : u16 = 0b1010111u16;
    pub const CUSTOM2: u16 = 0b1011011u16;
    pub const BRANCH: u16 = 0b1100011u16;
    pub const JALR: u16 = 0b1100111u16;
    pub const JAL: u16 = 0b1101111u16;
    pub const SYSTEM: u16 = 0b1110011u16;
    pub const CUSTOM3: u16 = 0b1111011u16;

    pub const NU_INVOKE: u16 = 0b001011011u16;
    pub const NU_INVOKE_IMM: u16 = 0b001111011u16;
    pub const NU_PAYLOAD: u16 = 0b011011011u16;
    pub const NU_COMPLETE: u16 = 0b101011011u16;
}

// TODO: use bitflags crate for this
#[derive(Debug)]
pub struct InstAction;

impl InstAction {
    pub const NONE: u32          = 0;
    pub const WRITE_REG: u32     = 1 << 0;
    pub const MEM_LOAD: u32      = 1 << 1;
    pub const MEM_STORE: u32     = 1 << 2;
    pub const SET_ABS_PC: u32    = 1 << 3;
    pub const SET_REL_PC: u32    = 1 << 4;
    pub const LINK: u32          = 1 << 5;
    pub const FENCE: u32         = 1 << 6;
    pub const SFU: u32           = 1 << 7;
    pub const CSR: u32           = 1 << 8;
}

macro_rules! f3_f7_mask {
    ($f3:expr, $f7:expr) => {
        ((($f3 as u16) << 7) | ($f7 as u16))
    };
}

macro_rules! print_and_execute {
    ($inst:expr, $operands:expr) => {{
        debug!("{} {:08x?}", $inst.0, $operands);
        $inst.1($operands)
    }};
}

#[macro_export]
macro_rules! print_and_unwrap {
    ($inst:expr) => {{
        debug!("{}", $inst.0);
        $inst.1
    }};
}


#[derive(Debug, FromPrimitive, Clone, Copy)]
pub enum SFUType {
    TMC    = 0,
    WSPAWN = 1,
    SPLIT  = 2,
    JOIN   = 3,
    BAR    = 4,
    PRED   = 5,
    KILL   = 6,
    ECALL  = 7,
}

#[derive(Debug, FromPrimitive, Clone, Copy, PartialEq)]
pub enum CSRType {
    RW  = 1,
    RS  = 2,
    RC  = 3,
    RWI = 5,
    RSI = 6,
    RCI = 7,
}

#[derive(Debug)]
pub struct InstImp<const N: usize> (
    pub &'static str,
    pub fn([u32; N]) -> u32,
);

#[derive(Debug)]
pub struct InstDef<T> (
    pub &'static str,
    pub T,
);

#[derive(Debug)]
pub struct ExecuteUnit;

impl ExecuteUnit {
    pub fn alu(issued: &IssuedInst, lane: usize) -> Option<u32> {
        fn check_zero(b: u32) -> bool {
            if b == 0 {
                panic!("divide by zero");
            } else {
                false
            }
        }

        const fn check_overflow(a: u32, b: i32) -> i32 {
            if (a == 0x8000_0000u32) && (b == -1) {
                1
            } else {
                b
            }
        }

        static OP_INSTS: phf::Map<u16, InstImp<2>> = phf_map! {
            0b000_0000000u16 => InstImp("add",    |[a, b]| { a.wrapping_add(b) }),
            0b000_0100000u16 => InstImp("sub",    |[a, b]| { (a as i32).wrapping_sub(b as i32) as u32 }),
            0b001_0000000u16 => InstImp("sll",    |[a, b]| { a << (b & 31) }),
            0b010_0000000u16 => InstImp("slt",    |[a, b]| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            0b011_0000000u16 => InstImp("sltu",   |[a, b]| { if a < b { 1 } else { 0 } }),
            0b100_0000000u16 => InstImp("xor",    |[a, b]| { a ^ b }),
            0b101_0000000u16 => InstImp("srl",    |[a, b]| { a >> (b & 31) }),
            0b101_0100000u16 => InstImp("sra",    |[a, b]| { ((a as i32) >> (b & 31)) as u32 }),
            0b110_0000000u16 => InstImp("or",     |[a, b]| { a | b }),
            0b111_0000000u16 => InstImp("and",    |[a, b]| { a & b }),
            0b000_0000001u16 => InstImp("mul",    |[a, b]| { a.wrapping_mul(b) }),
            0b001_0000001u16 => InstImp("mulh",   |[a, b]| { ((((a as i32) as i64).wrapping_mul((b as i32) as i64)) >> 32) as u32 }),
            0b010_0000001u16 => InstImp("mulhsu", |[a, b]| { ((((a as i32) as i64).wrapping_mul((b as u64) as i64)) >> 32) as u32 }),
            0b011_0000001u16 => InstImp("mulhu",  |[a, b]| { (((a as u64).wrapping_mul(b as u64)) >> 32) as u32 }),
            0b100_0000001u16 => InstImp("div",    |[a, b]| { if check_zero(b) { u32::MAX } else { ((a as i32) / check_overflow(a, b as i32)) as u32 } }),
            0b101_0000001u16 => InstImp("divu",   |[a, b]| { if check_zero(b) { u32::MAX } else { a / b } }),
            0b110_0000001u16 => InstImp("rem",    |[a, b]| { if check_zero(b) { a } else { ((a as i32) % check_overflow(a, b as i32)) as u32 } }),
            0b111_0000001u16 => InstImp("remu",   |[a, b]| { if check_zero(b) { a } else { a % b } }),
        };

        static OPIMM_F3_INSTS: phf::Map<u8, InstImp<2>> = phf_map! {
            0u8 => InstImp("addi",  |[a, b]| { a.wrapping_add(b) }),
            2u8 => InstImp("slti",  |[a, b]| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            3u8 => InstImp("sltiu", |[a, b]| { if a < b { 1 } else { 0 } }),
            4u8 => InstImp("xori",  |[a, b]| { a ^ b }),
            6u8 => InstImp("ori",   |[a, b]| { a | b }),
            7u8 => InstImp("andi",  |[a, b]| { a & b }),
        };

        static OPIMM_F3F7_INSTS: phf::Map<u16, InstImp<2>> = phf_map! {
            0b001_0000000u16 => InstImp("slli", |[a, b]| { a << (b & 31) }),
            0b101_0000000u16 => InstImp("srli", |[a, b]| { a >> (b & 31) }),
            0b101_0100000u16 => InstImp("srai", |[a, b]| { ((a as i32) >> (b & 31)) as u32 }),
        };

        let rd_data = match issued.opcode {
            Opcode::OP => {
                OP_INSTS.get(&(f3_f7_mask!(issued.f3, issued.f7))).and_then(|imp| {
                    Some(print_and_execute!(imp, [
                        issued.rs1_data[lane].unwrap(),
                        issued.rs2_data[lane].unwrap(),
                    ]))
                })
            }
            Opcode::OP_IMM => {
                OPIMM_F3_INSTS.get(&issued.f3).or_else(|| {
                    OPIMM_F3F7_INSTS.get(&(f3_f7_mask!(issued.f3, issued.f7)))
                }).and_then(|imp| {
                    Some(print_and_execute!(imp, [
                        issued.rs1_data[lane].unwrap(),
                        issued.imm32
                    ]))
                })
            }
            Opcode::AUIPC => {
                let imp = InstImp("auipc", |[a, b]| { a.wrapping_add(b) });
                Some(print_and_execute!(imp, [
                    issued.pc,
                    issued.imm32
                ]))
            }
            Opcode::LUI => {
                let imp = InstImp("lui", |[a]| { a << 12 });
                Some(print_and_execute!(imp, [issued.imm32]))
            }
            _ => { panic!("unreachable"); }
        };

        rd_data
    }

    pub fn fpu(issued_inst: &IssuedInst, lane: usize) -> Option<u32> {
        fn fp_op(a: u32, b: u32, op: fn(f32, f32) -> f32) -> u32 {
            let result = op(f32::from_bits(a), f32::from_bits(b)).to_bits();
            // info!("result of the fp operation is {:08x}", result);
            if [0xffc00000, 0x7fffffff, 0xffffffff].contains(&result) {
                0x7fc00000 // risc-v only ever generates this qNaNf
            } else {
                result
            }
        }

        fn fcmp_op(a: u32, b: u32, op: fn(f32, f32) -> bool) -> u32 {
            op(f32::from_bits(a), f32::from_bits(b)) as u32
        }

        fn fsgn_op(a: u32, b: u32, op: fn(f32, u32) -> f32) -> u32 {
            op(f32::from_bits(a), b).to_bits()
        }

        // NOTE: this really should just be the f32::minimum/maximum functions, but
        // they are unstable. we should swap that in when that goes into stable.
        fn fminmax(a: f32, b: f32, min: bool) -> f32 {
            if a.is_nan() { return b; }
            if b.is_nan() { return a; }
            if (a == 0f32) && (b == 0f32) {
                if (min && a.is_sign_negative()) || (!min && a.is_sign_positive()) {
                    a
                } else {
                    b
                }
            } else {
                if min {
                    a.min(b)
                } else {
                    a.max(b)
                }
            }
        }

        fn fcvt_saturate(a: u32, unsigned: bool) -> u32 {
            let f = f32::from_bits(a);
            if unsigned {
                f.to_u32().unwrap_or_else(|| {
                    if f < 0f32 {
                        0u32
                    } else {
                        u32::MAX
                    }
                })
            } else {
                f.to_i32().unwrap_or_else(|| {
                    if f < i32::MIN as f32 {
                        i32::MIN
                    } else {
                        i32::MAX
                    }
                }) as u32
            }
        }

        fn fclass(a: u32) -> u32 {
            let f = f32::from_bits(a);
            let conds = [
                f == f32::NEG_INFINITY,             // 0
                f  < 0f32 && f.is_normal(),         // 1
                f  < 0f32 && f.is_subnormal(),      // 2
                f == 0f32 && f.is_sign_negative(),  // 3
                f == 0f32 && f.is_sign_positive(),  // 4
                f  > 0f32 && f.is_subnormal(),      // 5
                f  > 0f32 && f.is_normal(),         // 6
                f == f32::INFINITY,                 // 7
                a == 0x7f800001, // sNaNf           // 8
                a == 0x7fc00000, // qNaNf           // 9
            ];
            (1 << conds.iter().position(|&c| c).unwrap()) as u32
        }

        static OPFP_F3F7_INSTS: phf::Map<u16, InstImp<2>> = phf_map! {
            0b000_0010000u16 => InstImp("fsgnj.s",  |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { -x.abs() } else { x.abs() } }) }),
            0b001_0010000u16 => InstImp("fsgnjn.s", |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { x.abs() } else { -x.abs() } }) }),
            0b010_0010000u16 => InstImp("fsgnjx.s", |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { -1.0 * x } else { x } }) }),
            0b000_0010100u16 => InstImp("fmin.s",   |[a, b]| { fp_op(a, b, |x, y| { fminmax(x, y, true) }) }),
            0b001_0010100u16 => InstImp("fmax.s",   |[a, b]| { fp_op(a, b, |x, y| { fminmax(x, y, false) }) }),
            0b010_1010000u16 => InstImp("feq.s",    |[a, b]| { fcmp_op(a, b, |x, y| { x == y }) }),
            0b001_1010000u16 => InstImp("flt.s",    |[a, b]| { fcmp_op(a, b, |x, y| { x < y }) }),
            0b000_1010000u16 => InstImp("fle.s",    |[a, b]| { fcmp_op(a, b, |x, y| { x <= y }) }),
            0b001_1110000u16 => InstImp("fclass.s", |[a, _b]| { fclass(a) }),
        };

        static OPFP_F7_INSTS: phf::Map<u8, InstImp<3>> = phf_map! {
            0b0000000u8 => InstImp("fadd.s",   |[a, b, _rs2_addr]| { fp_op(a, b, |x, y| { x + y }) }),
            0b0000100u8 => InstImp("fsub.s",   |[a, b, _rs2_addr]| { fp_op(a, b, |x, y| { x - y }) }),
            0b0001000u8 => InstImp("fmul.s",   |[a, b, _rs2_addr]| { fp_op(a, b, |x, y| { x * y }) }),
            0b0001100u8 => InstImp("fdiv.s",   |[a, b, _rs2_addr]| { fp_op(a, b, |x, y| { x / y }) }),
            0b0101100u8 => InstImp("fsqrt.s",  |[a, b, _rs2_addr]| { fp_op(a, b, |x, _y| { x.sqrt() }) }),
            0b1100000u8 => InstImp("fcvt.*.s", |[a, _b, rs2_addr]| { fcvt_saturate(a, rs2_addr > 0) }),
            0b1101000u8 => InstImp("fcvt.s.*", |[a, _b, rs2_addr]| { if rs2_addr > 0 { f32::to_bits(a as f32) } else { f32::to_bits(a as i32 as f32) } }),
        };

        let rd_data = match issued_inst.opcode {
            Opcode::OP_FP => {
                OPFP_F3F7_INSTS.get(&(f3_f7_mask!(issued_inst.f3, issued_inst.f7)))
                    .and_then(|imp| {
                        Some(print_and_execute!(imp, [
                            issued_inst.rs1_data[lane].unwrap(),
                            issued_inst.rs2_data[lane].unwrap(),
                        ]))
                    }).or_else(|| {
                        OPFP_F7_INSTS.get(&issued_inst.f7).and_then(|imp| {
                            Some(print_and_execute!(imp, [
                                issued_inst.rs1_data[lane].unwrap(),
                                issued_inst.rs2_data[lane].unwrap(),
                                issued_inst.rs2_addr as u32
                            ]))
                        })
                    })
            }
            Opcode::MADD | Opcode::MSUB | Opcode::NM_ADD | Opcode::NM_SUB => {
                let imp = match issued_inst.opcode {
                    Opcode::MADD => InstImp("fmadd.s", |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| { x + y }) }),
                    Opcode::MSUB => InstImp("fmsub.s", |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| { x - y}) }),
                    Opcode::NM_ADD => InstImp("fnmadd.s", |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {-x - y}) }),
                    Opcode::NM_SUB => InstImp("fnmsub.s", |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {-x + y}) }),
                    _ => { panic!() }
                };
                Some(print_and_execute!(imp, [
                    issued_inst.rs1_data[lane].unwrap(),
                    issued_inst.rs2_data[lane].unwrap(),
                    issued_inst.rs3_data[lane].unwrap(),
                ]))
            }
            _ => { panic!("unreachable"); }
        };

        rd_data
    }

    /// Returns Some(target PC) if branch is taken, None otherwise.
    pub fn branch(issued_inst: &IssuedInst, lane: usize) -> Option<u32> {
        let rs1 = issued_inst.rs1_data[lane].unwrap();
        let rs2 = issued_inst.rs2_data[lane].unwrap();
        let branch_offset = issued_inst.imm24 as u32;
        let branch_target = issued_inst.pc.wrapping_add(branch_offset);
        static INSTS: phf::Map<u8, InstDef<fn([u32; 2]) -> bool>> = phf_map! {
            0b000u8 => InstDef("beq",  |[a, b]| a == b),
            0b001u8 => InstDef("bne",  |[a, b]| a != b),
            0b100u8 => InstDef("blt",  |[a, b]| (a as i32) < (b as i32)),
            0b101u8 => InstDef("bge",  |[a, b]| (a as i32) >= (b as i32)),
            0b110u8 => InstDef("bltu", |[a, b]| a < b),
            0b111u8 => InstDef("bgeu", |[a, b]| a >= b),
        };
        let taken = INSTS.get(&issued_inst.f3).and_then(|imp| {
            Some(print_and_execute!(imp, [rs1, rs2]))
        }).expect("unimplemented branch instruction");
        taken.then_some(branch_target)
    }

    pub fn load(issued_inst: &IssuedInst, lane: usize, gmem: &RwLock<ToyMemory>) -> Option<u32> {
        // TODO: simplify this to have the phf_map only provide the instruction name
        static INSTS: phf::Map<u8, InstImp<2>> = phf_map! {
            0u8 => InstImp("lb",  |[a, b]| { a.wrapping_add(b) }),
            1u8 => InstImp("lh",  |[a, b]| { a.wrapping_add(b) }),
            2u8 => InstImp("lw",  |[a, b]| { a.wrapping_add(b) }),
            3u8 => InstImp("ld",  |[a, b]| { a.wrapping_add(b) }),
            4u8 => InstImp("lbu", |[a, b]| { a.wrapping_add(b) }),
            5u8 => InstImp("lhu", |[a, b]| { a.wrapping_add(b) }),
            6u8 => InstImp("lwu", |[a, b]| { a.wrapping_add(b) }),
        };

        let alu_result = INSTS.get(&issued_inst.f3).and_then(|imp| {
            Some(print_and_execute!(imp, [
                issued_inst.rs1_data[lane].unwrap(),
                issued_inst.imm32
            ]))
        }).expect("unimplemented");

        let load_size = issued_inst.f3 & 3;
        let load_addr = alu_result >> 2 << 2;
        assert_eq!(alu_result >> 2, (alu_result + (1 << load_size) - 1) >> 2, "misaligned load");

        let load_data_bytes = gmem.write().expect("lock poisoned").read::<4>(
            load_addr as usize).expect("store failed");

        let raw_load = u32::from_le_bytes(*load_data_bytes);
        let offset = ((alu_result & 3) * 8) as usize;
        let sext = !issued_inst.f3.bit(2);
        let opt_sext = |f: fn(u32) -> i32, x: u32| { if sext { f(x) as u32 } else { x } };
        let masked_load = match load_size {
            0 => opt_sext(sign_ext::<8>, raw_load.sel(7 + offset, offset)),   // load byte
            1 => opt_sext(sign_ext::<16>, raw_load.sel(15 + offset, offset)), // load half
            2 => raw_load,                                                    // load word
            _ => panic!("unimplemented load type"),
        };
        // info!("load f3={} M[0x{:08x}] -> raw 0x{:08x} masked 0x{:08x}",
        //         issued_inst.f3, load_addr, raw_load, masked_load);

        Some(masked_load)
    }

    pub fn store(issued_inst: &IssuedInst, lane: usize, gmem: &RwLock<ToyMemory>) -> Option<u32> {
        static INSTS: phf::Map<u8, InstImp<2>> = phf_map! {
            0u8 => InstImp("sb", |[a, imm]| { a.wrapping_add(imm) }),
            1u8 => InstImp("sh", |[a, imm]| { a.wrapping_add(imm) }),
            2u8 => InstImp("sw", |[a, imm]| { a.wrapping_add(imm) }),
            // 3u8 => InstImp("sd", |[a, imm]| { a.wrapping_add(imm) }),
        };

        let alu_result = INSTS.get(&issued_inst.f3).and_then(|imp| {
            Some(print_and_execute!(imp, [
                issued_inst.rs1_data[lane].unwrap(),
                issued_inst.imm24 as u32
            ]))
        }).expect("unimplemented");

        let mut gmem = gmem.write().expect("lock poisoned");
        let addr = alu_result as usize;
        let data = issued_inst.rs2_data[lane].unwrap().to_le_bytes();
        match issued_inst.f3 & 3 {
            0 => {
                gmem.write(addr, &data[0..1].to_vec())
            },
            1 => {
                gmem.write(addr, &data[0..2].to_vec())
            },
            2 => {
                gmem.write(addr, &data[0..4].to_vec())
            },
            _ => panic!("unimplemented store type"),
        }.expect("store failed");

        None
    }

    pub fn csr(issued_inst: &IssuedInst, lane: usize, csrf: &mut CSRFile) -> Option<u32> {
        let csr_type = print_and_unwrap!(match issued_inst.f3 {
            1 => InstDef("csrrw",  CSRType::RW),
            2 => InstDef("csrrs",  CSRType::RS),
            3 => InstDef("csrrc",  CSRType::RC),
            5 => InstDef("csrrwi", CSRType::RWI),
            6 => InstDef("csrrsi", CSRType::RSI),
            7 => InstDef("csrrci", CSRType::RCI),
            _ => panic!("unimplemented")
        });
        let new_val = match csr_type {
            CSRType::RW | CSRType::RS | CSRType::RC => {
                issued_inst.rs1_data[lane].unwrap()
            }
            CSRType::RWI | CSRType::RSI | CSRType::RCI => {
                // CSR immediate is stored in the same field as rs1_addr
                issued_inst.rs1_addr as u32
            }
        };
        let csrr = match csr_type {
            CSRType::RS | CSRType::RSI => new_val == 0,
            _ => false,
        };
        let addr = issued_inst.imm32;
        if [0xcc3, 0xcc4].contains(&addr) && !csrr {
            panic!("unimplemented thread mask write using csr");
        }
        let old_val = csrf.user_access(addr, new_val, csr_type);
        debug!("csr read address {:04x} => value {}", addr, old_val);

        Some(old_val)
    }

    pub fn sfu(issued_inst: &IssuedInst, wid: usize, first_lid: usize,
               rf: &mut [RegFile], scheduler: &mut Scheduler) {
        let insts = phf_map! {
            // sets thread mask to rs1[NT-1:0]
            0b000_0000000u16 => InstDef("vx_tmc",   SFUType::TMC),
            // spawns rs1 warps, except the executing warp, and set their pc's to rs2
            0b001_0000000u16 => InstDef("vx_wspawn",SFUType::WSPAWN),
            // collect rs1[0] for then mask. divergent if mask not all 0 or 1. write divergence back. set tmc, push else mask to ipdom
            0b010_0000000u16 => InstDef("vx_split", SFUType::SPLIT),
            // rs1[0] indicates divergence from split. pop ipdom and set tmc if divergent
            0b011_0000000u16 => InstDef("vx_join",  SFUType::JOIN),
            // rs1 indicates barrier id, rs2 indicates num warps participating in each core
            0b100_0000000u16 => InstDef("vx_bar",   SFUType::BAR),
            // sets thread mask to current tmask & then_mask, same rules as split. if no lanes take branch, set mask to rs2.
            0b101_0000000u16 => InstDef("vx_pred",  SFUType::PRED),
            // 0b000_0000001u16 => InstDef("vx_rast",  SFUType::),
            // signals the result of a test, used only for isa tests
        };
        let tohost_inst = InstDef("tohost", SFUType::ECALL);
        let sfu_type = if issued_inst.opcode == Opcode::SYSTEM {
            print_and_unwrap!(tohost_inst)
        } else {
            insts.get(&(f3_f7_mask!(issued_inst.f3, issued_inst.f7))).and_then(|imp| {
                Some(print_and_unwrap!(imp))
            }).expect("unimplemented sfu instruction")
        };

        scheduler.sfu(wid, first_lid, sfu_type, issued_inst,
            rf.iter().map(|lrf| lrf.read_gpr(issued_inst.rs1_addr)).collect(),
            rf.iter().map(|lrf| lrf.read_gpr(issued_inst.rs2_addr)).collect());
    }

    /// Collect source operand values from the regfile.
    pub fn collect(ibuf: &InstBufEntry, rf: &[RegFile]) -> IssuedInst {
        let decoded = ibuf.inst;
        let tmask = ibuf.tmask;

        let collect_source_reg = |rs_addr| {
            rf.iter().enumerate().map(|(lane, lrf)| {
                match tmask.bit(lane) {
                    true => Some(lrf.read_gpr(rs_addr)),
                    false => None,
                }
            }).collect::<Vec<_>>()
        };
        let rs1_data = collect_source_reg(decoded.rs1_addr);
        let rs2_data = collect_source_reg(decoded.rs2_addr);
        let rs3_data = collect_source_reg(decoded.rs3_addr);
        let rs4_data = collect_source_reg(decoded.rs4_addr);

        IssuedInst {
            opcode: decoded.opcode,
            rd_addr: decoded.rd_addr,
            f3: decoded.f3,
            rs1_addr: decoded.rs1_addr,
            rs2_addr: decoded.rs2_addr,
            rs3_addr: decoded.rs3_addr,
            rs4_addr: decoded.rs4_addr,
            rs1_data,
            rs2_data,
            rs3_data,
            rs4_data,
            f7: decoded.f7,
            imm32: decoded.imm32,
            imm24: decoded.imm24,
            pc: decoded.pc,
            raw: decoded.raw,
        }
    }

    #[inline]
    fn execute_lanes<F>(mut func: F, tmask: u32, rf: &mut [RegFile]) -> Vec<Option<u32>>
    where
        F: FnMut(usize) -> Option<u32>
    {
        rf.iter_mut().enumerate().map(|(lane, _)| {
            match tmask.bit(lane) {
                true => func(lane),
                false => None,
            }
        }).collect()
    }

    pub fn execute(issued: IssuedInst, cid: usize, wid: usize, tmask: u32,
                   rf: &mut [RegFile], csrf: &mut [CSRFile],
                   scheduler: &mut Scheduler, neutrino: &mut Neutrino, gmem: &RwLock<ToyMemory>) -> Writeback {

        let num_lanes = rf.len();
        // lane id of first active thread
        let first_lid = tmask.trailing_zeros() as usize;

        // debug!("execute pc 0x{:08x} {}", issued.pc, issued);

        let empty = vec![None::<u32>; num_lanes];
        let collected_rds = match issued.opcode {
            Opcode::OP | Opcode::OP_IMM | Opcode::LUI | Opcode::AUIPC => {
                Self::execute_lanes(|lane| ExecuteUnit::alu(&issued, lane), tmask, rf)
            }
            Opcode::OP_FP | Opcode::MADD | Opcode::MSUB | Opcode::NM_ADD | Opcode::NM_SUB => {
                Self::execute_lanes(|lane| ExecuteUnit::fpu(&issued, lane), tmask, rf)
            }
            Opcode::BRANCH => {
                if let Some(target) = ExecuteUnit::branch(&issued, first_lid) {
                    scheduler.take_branch(wid, target);
                }
                empty
            }
            Opcode::JAL => {
                scheduler.take_branch(wid, issued.pc.wrapping_add(issued.imm32));
                Self::execute_lanes(|_| { Some(issued.pc + 8) }, tmask, rf)
            }
            Opcode::JALR => {
                let target = issued.rs1_data[first_lid].unwrap().wrapping_add(issued.imm32);
                scheduler.take_branch(wid, target);
                Self::execute_lanes(|_| { Some(issued.pc + 8) }, tmask, rf)
            }
            Opcode::LOAD => {
                Self::execute_lanes(|lane| ExecuteUnit::load(&issued, lane, gmem), tmask, rf)
            }
            Opcode::STORE => {
                Self::execute_lanes(|lane| ExecuteUnit::store(&issued, lane, gmem), tmask, rf)
            }
            Opcode::MISC_MEM => {
                let imp = match issued.f3 {
                    0 => InstDef("fence",   0),
                    1 => InstDef("fence.i", 1),
                    _ => panic!("unimplemented"),
                };
                print_and_unwrap!(imp);
                // TODO fence
                empty
            }
            Opcode::SYSTEM => {
                if issued.f3 == 0 { // tohost
                    // FIXME: tmask?
                    ExecuteUnit::sfu(&issued, wid, first_lid, rf, scheduler);
                    empty
                } else { // csr
                    let wb = csrf.iter_mut().enumerate()
                        .map(|(lane, lcsrf)| {
                            match tmask.bit(lane) {
                                true => ExecuteUnit::csr(&issued, lane, lcsrf),
                                false => None,
                            }
                        }).collect();
                    wb
                }
            }
            Opcode::CUSTOM0 => {
                // FIXME: tmask?
                ExecuteUnit::sfu(&issued, wid, first_lid, rf, scheduler);
                empty
            }
            Opcode::CUSTOM1 => {
                // 0b000_0000000u16 => InstImp("vx_tex",  |[a, b, c]| { }),
                // 0b001_0000000u16 => InstImp("vx_cmov", |[a, b, c]| { }),
                // 0b001_0000001u16 => InstImp("vx_rop",  |[a, b, c]| { }),
                todo!("graphics ops unimplemented")
            }
            Opcode::CUSTOM2 => {
                neutrino.execute(&issued, cid, wid, tmask, &mut rf[first_lid]);
                empty
            }

            _ => { panic!("unimplemented opcode 0x{:x}", issued.opcode); }
        };

        let issued_rd_addr = issued.rd_addr;
        Writeback {
            inst: issued,
            tmask,
            rd_addr: issued_rd_addr,
            rd_data: collected_rds,
        }
    }
}
