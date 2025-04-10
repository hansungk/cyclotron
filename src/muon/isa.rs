use std::fmt::Debug;

use log::{error, info};
use num_derive::FromPrimitive;
use num_traits::real::Real;
use num_traits::ToPrimitive;
pub use num_traits::WrappingAdd;
use crate::{make_bitpat, muon::decode::DecodedInst};
use crate::utils::BitSlice;

#[derive(Debug, FromPrimitive, Clone)]
pub enum Opcode {
    Load    = 0b0000011,
    LoadFp  = 0b0000111,
    Custom0 = 0b0001011,
    MiscMem = 0b0001111,
    OpImm   = 0b0010011,
    Auipc   = 0b0010111,
//  OpImm32 = 0b0011011,
    Store   = 0b0100011,
    StoreFp = 0b0100111,
    Custom1 = 0b0101011,
//  Amo     = 0b0101111,
    Op      = 0b0110011,
    Lui     = 0b0110111,
    Op32    = 0b0111011,
    Madd    = 0b1000011,
    Msub    = 0b1000111,
    NmSub   = 0b1001011,
    NmAdd   = 0b1001111,
    OpFp    = 0b1010011,
//  OpV     = 0b1010111,
    Custom2 = 0b1011011,
    Branch  = 0b1100011,
    Jalr    = 0b1100111,
    Jal     = 0b1101111,
    System  = 0b1110011,
    Custom3 = 0b1111011,
}

impl From<&Opcode> for u16 {
    fn from(value: &Opcode) -> Self {
        value.clone() as u16
    }
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
pub struct InstImp<const N: usize> {
    name: &'static str,
    mask: u64,
    pattern: u64,
    actions: u32,
    op: fn([u32; N]) -> u32,
}

impl InstImp<0> {
    pub const fn nul_f3(name: &'static str, opcode: Opcode, f3: u8, actions: u32, op: fn([u32; 0]) -> u32) -> InstImp<0> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64
        );

        InstImp::<0> { name, mask, pattern, actions, op }
    }

    pub const fn nul_f3_f7(name: &'static str, opcode: Opcode, f3: u8, f7: u8, actions: u32, op: fn([u32; 0]) -> u32) -> InstImp<0> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64,
            F7_MASK => f7 as u64
        );

        InstImp::<0> { name, mask, pattern, actions, op }
    }

    pub const fn una(name: &'static str, opcode: Opcode, actions: u32, op: fn([u32; 1]) -> u32) -> InstImp<1> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64
        );

        InstImp::<1> { name, mask, pattern, actions, op }
    }

    pub const fn bin_f3_f7(name: &'static str, opcode: Opcode, f3: u8, f7: u8, actions: u32, op: fn([u32; 2]) -> u32) -> InstImp<2> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64,
            F7_MASK => f7 as u64
        );

        InstImp::<2> { name, mask, pattern, actions, op }
    }

    pub const fn bin_f3(name: &'static str, opcode: Opcode, f3: u8, actions: u32, op: fn([u32; 2]) -> u32) -> InstImp<2> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64
        );

        InstImp::<2> { name, mask, pattern, actions, op }
    }

    pub const fn bin_f7(name: &'static str, opcode: Opcode, f7: u8, actions: u32, op: fn([u32; 2]) -> u32) -> InstImp<2> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F7_MASK => f7 as u64
        );

        InstImp::<2> { name, mask, pattern, actions, op }
    }

    pub const fn bin(name: &'static str, opcode: Opcode, actions: u32, op: fn([u32; 2]) -> u32) -> InstImp<2> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64
        );

        InstImp::<2> { name, mask, pattern, actions, op }
    }

    pub const fn ter_f3(name: &'static str, opcode: Opcode, f3: u8, actions: u32, op: fn([u32; 3]) -> u32) -> InstImp<3> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64
        );

        InstImp::<3> { name, mask, pattern, actions, op }
    }

    pub const fn ter_f3_f2(name: &'static str, opcode: Opcode, f3: u8, f2: u8, actions: u32, op: fn([u32; 3]) -> u32) -> InstImp<3> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64,
            F3_MASK => f3 as u64,
            F7_MASK => f2 as u64
        );

        InstImp::<3> { name, mask, pattern, actions, op }
    }

    pub const fn ter(name: &'static str, opcode: Opcode, actions: u32, op: fn([u32; 3]) -> u32) -> InstImp<3> {
        let (mask, pattern) = make_bitpat!(
            OPCODE_MASK => opcode as u64
        );

        InstImp::<3> { name, mask, pattern, actions, op }
    }
}

#[derive(Debug)]
pub struct InstGroupVariant<const N: usize> {
    pub insts: &'static [InstImp<N>],
    pub get_operands: fn(&DecodedInst) -> [u32; N],
}

impl<const N: usize> InstGroupVariant<N> {
    // returns Some((alu result, actions)) if opcode, f3 and f7 matches
    fn execute(&self, req: &DecodedInst) -> Option<(&'static str, u32, u32)> {
        let operands = (self.get_operands)(&req);

        self.insts.iter().map(|inst| {
            (inst.mask & req.raw == inst.pattern)
                .then(|| (inst.op)(operands))
                .map(|alu| (inst.name, alu, inst.actions))
        }).fold(None, Option::or)
    }
}

#[derive(Debug)]
pub enum InstGroup {
    Nullary(InstGroupVariant<0>),
    Unary(InstGroupVariant<1>),
    Binary(InstGroupVariant<2>),
    Ternary(InstGroupVariant<3>),
}

impl InstGroup {
    pub fn execute(&self, req: &DecodedInst) -> Option<(&'static str, u32, u32)> {
        match self {
            InstGroup::Nullary(x) => { x.execute(req) }
            InstGroup::Unary(x) => { x.execute(req) }
            InstGroup::Binary(x) => { x.execute(req) }
            InstGroup::Ternary(x) => { x.execute(req) }
        }
    }
}



// TODO: make this all constant
#[derive(Debug)]
pub struct ISA;
impl ISA {

    fn check_zero(b: u32) -> bool {
        if b == 0 {
            error!("divide by zero");
            true
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

    #[allow(non_upper_case_globals)]
    #[allow(unused_variables)]
    pub fn get_insts() -> &'static [InstGroup] {
        const sfu_inst_imps: &'static [InstImp<0>] = &[
            InstImp::nul_f3("csrrw",  Opcode::System, 1, InstAction::CSR, |_| CSRType::RW as u32),
            InstImp::nul_f3("csrrs",  Opcode::System, 2, InstAction::CSR, |_| CSRType::RS as u32),
            InstImp::nul_f3("csrrc",  Opcode::System, 3, InstAction::CSR, |_| CSRType::RC as u32),
            InstImp::nul_f3("csrrwi", Opcode::System, 5, InstAction::CSR, |_| CSRType::RWI as u32),
            InstImp::nul_f3("csrrsi", Opcode::System, 6, InstAction::CSR, |_| CSRType::RSI as u32),
            InstImp::nul_f3("csrrci", Opcode::System, 7, InstAction::CSR, |_| CSRType::RCI as u32),
            // only support test pass/fail ecall
            InstImp::nul_f3("tohost", Opcode::System, 0, InstAction::SFU, |_| SFUType::ECALL as u32),
            // sets thread mask to rs1[NT-1:0]
            InstImp::nul_f3_f7("vx_tmc",    Opcode::Custom0, 0, 0, InstAction::SFU, |_| SFUType::TMC as u32),
            // spawns rs1 warps, except the executing warp, and set their pc's to rs2
            InstImp::nul_f3_f7("vx_wspawn", Opcode::Custom0, 1, 0, InstAction::SFU, |_| SFUType::WSPAWN as u32),
            // collect rs1[0] for then mask. divergent if mask not all 0 or 1. write divergence back. set tmc, push else mask to ipdom
            InstImp::nul_f3_f7("vx_split",  Opcode::Custom0, 2, 0, InstAction::SFU, |_| SFUType::SPLIT as u32),
            // rs1[0] indicates divergence from split. pop ipdom and set tmc if divergent
            InstImp::nul_f3_f7("vx_join",   Opcode::Custom0, 3, 0, InstAction::SFU, |_| SFUType::JOIN as u32),
            // rs1 indicates barrier id, rs2 indicates num warps participating in each core
            InstImp::nul_f3_f7("vx_bar",    Opcode::Custom0, 4, 0, InstAction::SFU, |_| SFUType::BAR as u32),
            // sets thread mask to current tmask & then_mask, same rules as split. if no lanes take branch, set mask to rs2.
            InstImp::nul_f3_f7("vx_pred",   Opcode::Custom0, 5, 0, InstAction::SFU, |_| SFUType::PRED as u32),
            InstImp::nul_f3_f7("vx_rast",   Opcode::Custom0, 0, 1, InstAction::SFU | InstAction::WRITE_REG, |_| todo!()),
        ];
        const sfu_insts: InstGroupVariant<0> = InstGroupVariant {
            insts: sfu_inst_imps,
            get_operands: |_| [],
        };

        fn fp_op(a: u32, b: u32, op: fn(f32, f32) -> f32) -> u32 {
            let result = op(f32::from_bits(a), f32::from_bits(b)).to_bits();
            info!("result of the fp operation is {:08x}", result);
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
                if (min) {
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

        const fpu_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin_f7("fadd.s",  Opcode::OpFp,  0, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { x + y }) }),
            InstImp::bin_f7("fsub.s",  Opcode::OpFp,  4, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { x - y }) }),
            InstImp::bin_f7("fmul.s",  Opcode::OpFp,  8, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { x * y }) }),
            InstImp::bin_f7("fdiv.s",  Opcode::OpFp, 12, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { x / y }) }),
            InstImp::bin_f7("fsqrt.s", Opcode::OpFp, 44, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { x.sqrt() }) }),

            InstImp::bin_f3_f7("fsgnj.s",   Opcode::OpFp, 0,  16, InstAction::WRITE_REG, |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { -x.abs() } else { x.abs() } }) }),
            InstImp::bin_f3_f7("fsgnjn.s",  Opcode::OpFp, 1,  16, InstAction::WRITE_REG, |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { x.abs() } else { -x.abs() } }) }),
            InstImp::bin_f3_f7("fsgnjx.s",  Opcode::OpFp, 2,  16, InstAction::WRITE_REG, |[a, b]| { fsgn_op(a, b, |x, y| { if y.bit(31) { -1.0 * x } else { x } }) }),
            InstImp::bin_f3_f7("fmin.s",    Opcode::OpFp, 0,  20, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { fminmax(x, y, true) }) }),
            InstImp::bin_f3_f7("fmax.s",    Opcode::OpFp, 1,  20, InstAction::WRITE_REG, |[a, b]| { fp_op(a, b, |x, y| { fminmax(x, y, false) }) }),
            InstImp::bin_f3_f7("feq.s",     Opcode::OpFp, 2,  80, InstAction::WRITE_REG, |[a, b]| { fcmp_op(a, b, |x, y| { x == y }) }),
            InstImp::bin_f3_f7("flt.s",     Opcode::OpFp, 1,  80, InstAction::WRITE_REG, |[a, b]| { fcmp_op(a, b, |x, y| { x < y }) }),
            InstImp::bin_f3_f7("fle.s",     Opcode::OpFp, 0,  80, InstAction::WRITE_REG, |[a, b]| { fcmp_op(a, b, |x, y| { x <= y }) }),

            InstImp::bin_f3_f7("fclass.s",  Opcode::OpFp, 1, 112, InstAction::WRITE_REG, |[a, b]| { fclass(a) }),
            // these are just regular mv's, shouldn't be compiled
            InstImp::bin_f3_f7("fmv.x.w",   Opcode::OpFp, 0, 112, InstAction::WRITE_REG, |[a, b]| { todo!() }),
            InstImp::bin_f3_f7("fmv.w.x",   Opcode::OpFp, 0, 120, InstAction::WRITE_REG, |[a, b]| { todo!() }),
        ];
        const fpu_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: fpu_inst_imps,
            get_operands: |req| [req.rs1, req.rs2],
        };

        const fcvt_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin_f7("fcvt.*.s",     Opcode::OpFp,     96, InstAction::WRITE_REG, |[a, b]| { fcvt_saturate(a, b > 0) }),
            InstImp::bin_f7("fcvt.s.*",     Opcode::OpFp,    104, InstAction::WRITE_REG, |[a, b]| { if b > 0 { f32::to_bits(a as f32) } else { f32::to_bits(a as i32 as f32) } }),
        ];

        const fcvt_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: fcvt_inst_imps,
            get_operands: |req| [req.rs1, req.rs2_addr as u32],
        };

        const fm_inst_imps: &'static [InstImp<3>] = &[
            InstImp::ter("fmadd.s",   Opcode::Madd,  InstAction::WRITE_REG, |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {x + y}) }),
            InstImp::ter("fmsub.s",   Opcode::Msub,  InstAction::WRITE_REG, |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {x - y}) }),
            InstImp::ter("fnmadd.s",  Opcode::NmAdd, InstAction::WRITE_REG, |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {-x - y}) }),
            InstImp::ter("fnmsub.s",  Opcode::NmSub, InstAction::WRITE_REG, |[a, b, c]| { fp_op(fp_op(a, b, |x, y| { x * y }), c, |x, y| {-x + y}) }),
        ];

        const fm_insts: InstGroupVariant<3> = InstGroupVariant {
            insts: fm_inst_imps,
            get_operands: |req| [req.rs1, req.rs2, req.rs3],
        };

        const r3_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin_f3_f7("add",  Opcode::Op, 0,  0, InstAction::WRITE_REG, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3_f7("sub",  Opcode::Op, 0, 32, InstAction::WRITE_REG, |[a, b]| { (a as i32).wrapping_sub(b as i32) as u32 }),
            InstImp::bin_f3_f7("sll",  Opcode::Op, 1,  0, InstAction::WRITE_REG, |[a, b]| { a << (b & 31) }),
            InstImp::bin_f3_f7("slt",  Opcode::Op, 2,  0, InstAction::WRITE_REG, |[a, b]| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            InstImp::bin_f3_f7("sltu", Opcode::Op, 3,  0, InstAction::WRITE_REG, |[a, b]| { if a < b { 1 } else { 0 } }),
            InstImp::bin_f3_f7("xor",  Opcode::Op, 4,  0, InstAction::WRITE_REG, |[a, b]| { a ^ b }),
            InstImp::bin_f3_f7("srl",  Opcode::Op, 5,  0, InstAction::WRITE_REG, |[a, b]| { a >> (b & 31) }),
            InstImp::bin_f3_f7("sra",  Opcode::Op, 5, 32, InstAction::WRITE_REG, |[a, b]| { ((a as i32) >> (b & 31)) as u32 }),
            InstImp::bin_f3_f7("or",   Opcode::Op, 6,  0, InstAction::WRITE_REG, |[a, b]| { a | b }),
            InstImp::bin_f3_f7("and",  Opcode::Op, 7,  0, InstAction::WRITE_REG, |[a, b]| { a & b }),

            InstImp::bin_f3_f7("mul",    Opcode::Op, 0, 1, InstAction::WRITE_REG, |[a, b]| { a.wrapping_mul(b) }),
            InstImp::bin_f3_f7("mulh",   Opcode::Op, 1, 1, InstAction::WRITE_REG, |[a, b]| { ((((a as i32) as i64).wrapping_mul((b as i32) as i64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("mulhsu", Opcode::Op, 2, 1, InstAction::WRITE_REG, |[a, b]| { ((((a as i32) as i64).wrapping_mul((b as u64) as i64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("mulhu",  Opcode::Op, 3, 1, InstAction::WRITE_REG, |[a, b]| { (((a as u64).wrapping_mul(b as u64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("div",    Opcode::Op, 4, 1, InstAction::WRITE_REG, |[a, b]| { if ISA::check_zero(b) { u32::MAX } else { ((a as i32) / ISA::check_overflow(a, b as i32)) as u32 } }),
            InstImp::bin_f3_f7("divu",   Opcode::Op, 5, 1, InstAction::WRITE_REG, |[a, b]| { if ISA::check_zero(b) { u32::MAX } else { a / b } }),
            InstImp::bin_f3_f7("rem",    Opcode::Op, 6, 1, InstAction::WRITE_REG, |[a, b]| { if ISA::check_zero(b) { a } else { ((a as i32) % ISA::check_overflow(a, b as i32)) as u32 } }),
            InstImp::bin_f3_f7("remu",   Opcode::Op, 7, 1, InstAction::WRITE_REG, |[a, b]| { if ISA::check_zero(b) { a } else { a % b } }),
        ];
        const r3_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: r3_inst_imps,
            get_operands: |req| [req.rs1, req.rs2],
        };

        const r4_inst_imps: &'static [InstImp<3>] = &[
            InstImp::ter_f3_f2("vx_tex",  Opcode::Custom1, 0, 0, InstAction::WRITE_REG, |[a, b, c]| { todo!() }),
            InstImp::ter_f3_f2("vx_cmov", Opcode::Custom1, 1, 0, InstAction::WRITE_REG, |[a, b, c]| { todo!() }),
            InstImp::ter_f3_f2("vx_rop",  Opcode::Custom1, 1, 1, InstAction::NONE,      |[a, b, c]| { todo!() }),
        ];
        const r4_insts: InstGroupVariant<3> = InstGroupVariant {
            insts: r4_inst_imps,
            get_operands: |req| [req.rs1, req.rs2, req.rs3],
        };

        const i2_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin_f3("lb",  Opcode::Load, 0, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("lh",  Opcode::Load, 1, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("lw",  Opcode::Load, 2, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("ld",  Opcode::Load, 3, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("lbu", Opcode::Load, 4, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("lhu", Opcode::Load, 5, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3("lwu", Opcode::Load, 6, InstAction::MEM_LOAD, |[a, b]| { a.wrapping_add(b) }),

            InstImp::bin_f3("fence",   Opcode::MiscMem, 0, InstAction::FENCE, |[a, b]| { 0 }),
            InstImp::bin_f3("fence.i", Opcode::MiscMem, 1, InstAction::FENCE, |[a, b]| { 1 }),

            InstImp::bin_f3   ("addi", Opcode::OpImm, 0,     InstAction::WRITE_REG, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin_f3_f7("slli", Opcode::OpImm, 1,  0, InstAction::WRITE_REG, |[a, b]| { a << (b & 31) }),
            InstImp::bin_f3   ("slti", Opcode::OpImm, 2,     InstAction::WRITE_REG, |[a, b]| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            InstImp::bin_f3  ("sltiu", Opcode::OpImm, 3,     InstAction::WRITE_REG, |[a, b]| { if a < b { 1 } else { 0 } }),
            InstImp::bin_f3   ("xori", Opcode::OpImm, 4,     InstAction::WRITE_REG, |[a, b]| { a ^ b }),
            InstImp::bin_f3_f7("srli", Opcode::OpImm, 5,  0, InstAction::WRITE_REG, |[a, b]| { a >> (b & 31) }),
            InstImp::bin_f3_f7("srai", Opcode::OpImm, 5, 32, InstAction::WRITE_REG, |[a, b]| { ((a as i32) >> (b & 31)) as u32 }),
            InstImp::bin_f3    ("ori", Opcode::OpImm, 6,     InstAction::WRITE_REG, |[a, b]| { a | b }),
            InstImp::bin_f3   ("andi", Opcode::OpImm, 7,     InstAction::WRITE_REG, |[a, b]| { a & b }),

            InstImp::bin_f3("jalr", Opcode::Jalr, 0, InstAction::SET_ABS_PC | InstAction::LINK, |[a, b]| { a.wrapping_add(b) }),
        ];
        const i2_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: i2_inst_imps,
            get_operands: |req| [req.rs1, req.imm32 as u32],
        };

        // does not return anything
        const s_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin_f3("sb", Opcode::Store, 0, InstAction::MEM_STORE, |[a, imm]| { a.wrapping_add(imm) }),
            InstImp::bin_f3("sh", Opcode::Store, 1, InstAction::MEM_STORE, |[a, imm]| { a.wrapping_add(imm) }),
            InstImp::bin_f3("sw", Opcode::Store, 2, InstAction::MEM_STORE, |[a, imm]| { a.wrapping_add(imm) }),
            /* InstImp::bin_f3("sd", Opcode::Store, 3, InstAction::MEM_STORE, |a, imm| { a + imm }), */
        ];
        const s_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: s_inst_imps,
            get_operands: |req| [req.rs1, req.imm24 as u32],
        };

        // binary op returns branch offset if taken, 0 if not
        const sb_inst_imps: &'static [InstImp<3>] = &[
            InstImp::ter_f3("beq",  Opcode::Branch, 0, InstAction::SET_REL_PC, |[a, b, imm]| { if a == b { imm } else { 0 }  }),
            InstImp::ter_f3("bne",  Opcode::Branch, 1, InstAction::SET_REL_PC, |[a, b, imm]| { if a != b { imm } else { 0 }  }),
            InstImp::ter_f3("blt",  Opcode::Branch, 4, InstAction::SET_REL_PC, |[a, b, imm]| { if (a as i32) < (b as i32) { imm } else { 0 }  }),
            InstImp::ter_f3("bge",  Opcode::Branch, 5, InstAction::SET_REL_PC, |[a, b, imm]| { if (a as i32) >= (b as i32) { imm } else { 0 }  }),
            InstImp::ter_f3("bltu", Opcode::Branch, 6, InstAction::SET_REL_PC, |[a, b, imm]| { if a < b { imm } else { 0 }  }),
            InstImp::ter_f3("bgeu", Opcode::Branch, 7, InstAction::SET_REL_PC, |[a, b, imm]| { if a >= b { imm } else { 0 }  }),
        ];
        const sb_insts: InstGroupVariant<3> = InstGroupVariant {
            insts: sb_inst_imps,
            get_operands: |req| [req.rs1, req.rs2, req.imm24 as u32],
        };

        const pcrel_inst_imps: &'static [InstImp<2>] = &[
            InstImp::bin("auipc", Opcode::Auipc, InstAction::WRITE_REG, |[a, b]| { a.wrapping_add(b) }),
            InstImp::bin("jal", Opcode::Jal, InstAction::SET_ABS_PC| InstAction::LINK, |[a, b]| { a.wrapping_add(b) }),
        ];
        const pcrel_insts: InstGroupVariant<2> = InstGroupVariant {
            insts: pcrel_inst_imps,
            get_operands: |req| [req.pc, req.imm32 as u32],
        };

        const lui_inst_imp: &'static [InstImp<1>] = &[
            InstImp::una("lui", Opcode::Lui, InstAction::WRITE_REG, |[a]| { a }),
        ];
        const lui_inst: InstGroupVariant<1> = InstGroupVariant {
            insts: lui_inst_imp,
            get_operands: |req| [(req.imm32 as u32) << 12],
        };

        const INST_GROUPS: &[InstGroup] = &[
            InstGroup::Nullary(sfu_insts),
            InstGroup::Unary(lui_inst),
            InstGroup::Binary(r3_insts),
            InstGroup::Binary(i2_insts),
            InstGroup::Binary(s_insts),
            InstGroup::Binary(fpu_insts),
            InstGroup::Binary(fcvt_insts),
            InstGroup::Binary(pcrel_insts),
            InstGroup::Ternary(r4_insts),
            InstGroup::Ternary(sb_insts),
            InstGroup::Ternary(fm_insts)
        ];

        INST_GROUPS
    }
}