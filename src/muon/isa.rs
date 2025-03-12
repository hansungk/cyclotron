use std::fmt::Debug;

use log::error;
use num_derive::FromPrimitive;
pub use num_traits::WrappingAdd;
use crate::muon::decode::DecodedInst;

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
//  Madd    = 0b1000011,
//  Msub    = 0b1000111,
//  NmSub   = 0b1001011,
//  NmAdd   = 0b1001111,
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

pub trait OpImp<const N: usize>: Debug {
    fn run(&self, operands: [u32; N]) -> u32;
}

impl OpImp<0> for fn() -> u32 {
    fn run(&self, _: [u32; 0]) -> u32 {
        self()
    }
}

impl OpImp<1> for fn(u32) -> u32 {
    fn run(&self, operands: [u32; 1]) -> u32 {
        self(operands[0])
    }
}

impl OpImp<2> for fn(u32, u32) -> u32 {
    fn run(&self, operands: [u32; 2]) -> u32 {
        self(operands[0], operands[1])
    }
}

impl OpImp<3> for fn(u32, u32, u32) -> u32 {
    fn run(&self, operands: [u32; 3]) -> u32 {
        self(operands[0], operands[1], operands[2])
    }
}

#[derive(Debug)]
pub struct InstImp<const N: usize> {
    name: String,
    opcode: Opcode,
    f3: Option<u8>,
    f7: Option<u8>,
    actions: u32,
    op: Box<dyn OpImp<N>>,
}

impl InstImp<0> {
    pub fn nul_f3(name: &str, opcode: Opcode, f3: u8, actions: u32, op: fn() -> u32) -> InstImp<0> {
        InstImp::<0> { name: name.to_string(), opcode, f3: Some(f3), f7: None, actions, op: Box::new(op) }
    }

    pub fn nul_f3_f7(name: &str, opcode: Opcode, f3: u8, f7: u8, actions: u32, op: fn() -> u32) -> InstImp<0> {
        InstImp::<0> { name: name.to_string(), opcode, f3: Some(f3), f7: Some(f7), actions, op: Box::new(op) }
    }

    pub fn una(name: &str, opcode: Opcode, actions: u32, op: fn(u32) -> u32) -> InstImp<1> {
        InstImp::<1> { name: name.to_string(), opcode, f3: None, f7: None, actions, op: Box::new(op) }
    }

    pub fn bin_f3_f7(name: &str, opcode: Opcode, f3: u8, f7: u8, actions: u32, op: fn(u32, u32) -> u32) -> InstImp<2> {
        InstImp::<2> { name: name.to_string(), opcode, f3: Some(f3), f7: Some(f7), actions, op: Box::new(op) }
    }

    pub fn bin_f3(name: &str, opcode: Opcode, f3: u8, actions: u32, op: fn(u32, u32) -> u32) -> InstImp<2> {
        InstImp::<2> { name: name.to_string(), opcode, f3: Some(f3), f7: None, actions, op: Box::new(op) }
    }

    pub fn bin(name: &str, opcode: Opcode, actions: u32, op: fn(u32, u32) -> u32) -> InstImp<2> {
        InstImp::<2> { name: name.to_string(), opcode, f3: None, f7: None, actions, op: Box::new(op) }
    }

    pub fn ter_f3(name: &str, opcode: Opcode, f3: u8, actions: u32, op: fn(u32, u32, u32) -> u32) -> InstImp<3> {
        InstImp::<3> { name: name.to_string(), opcode, f3: Some(f3), f7: None, actions, op: Box::new(op) }
    }

    pub fn ter_f3_f2(name: &str, opcode: Opcode, f3: u8, f2: u8, actions: u32, op: fn(u32, u32, u32) -> u32) -> InstImp<3> {
        InstImp::<3> { name: name.to_string(), opcode, f3: Some(f3), f7: Some(f2), actions, op: Box::new(op) }
    }
}

#[derive(Debug)]
pub struct InstGroupVariant<const N: usize> {
    pub insts: Vec<InstImp<N>>,
    pub get_operands: fn(&DecodedInst) -> [u32; N],
}

impl<const N: usize> InstGroupVariant<N> {
    // returns Some((alu result, actions)) if opcode, f3 and f7 matches
    fn execute(&self, req: &DecodedInst) -> Option<(String, u32, u32)> {
        let operands = (self.get_operands)(&req);

        self.insts.iter().map(|inst| {
            match inst {
                InstImp { opcode, f3: Some(f3), f7: Some(f7), op, .. } => {
                    (req.opcode == opcode.into() && (req.f3 == *f3) && (req.f7 == *f7)).then(|| op.run(operands))
                },
                InstImp { opcode, f3: Some(f3), f7: _, op, .. } => {
                    (req.opcode == opcode.into() && req.f3 == *f3).then(|| op.run(operands))
                },
                InstImp { opcode, f3: _, f7: _, op, .. } => {
                    (req.opcode == opcode.into()).then(|| op.run(operands))
                },
            }.map(|alu| (inst.name.clone(), alu, inst.actions))
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
    pub fn execute(&self, req: &DecodedInst) -> Option<(String, u32, u32)> {
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

    fn check_zero(x: u32) -> u32 {
        if x == 0 {
            error!("divide by zero")
        }
        x
    }

    #[allow(unused_variables)]
    pub fn get_insts() -> Vec<Box<InstGroup>> {
        let sfu_inst_imps: Vec<InstImp<0>>  = vec![
            InstImp::nul_f3("csrrw",  Opcode::System, 1, InstAction::CSR, || CSRType::RW as u32),
            InstImp::nul_f3("csrrs",  Opcode::System, 2, InstAction::CSR, || CSRType::RS as u32),
            InstImp::nul_f3("csrrc",  Opcode::System, 3, InstAction::CSR, || CSRType::RC as u32),
            InstImp::nul_f3("csrrwi", Opcode::System, 5, InstAction::CSR, || CSRType::RWI as u32),
            InstImp::nul_f3("csrrsi", Opcode::System, 6, InstAction::CSR, || CSRType::RSI as u32),
            InstImp::nul_f3("csrrci", Opcode::System, 7, InstAction::CSR, || CSRType::RCI as u32),
            // only support test pass/fail ecall
            InstImp::nul_f3("ecall",  Opcode::System, 0, InstAction::SFU, || SFUType::ECALL as u32),
            // sets thread mask to rs1[NT-1:0]
            InstImp::nul_f3_f7("vx_tmc",    Opcode::Custom0, 0, 0, InstAction::SFU, || SFUType::TMC as u32),
            // spawns rs1 warps, except the executing warp, and set their pc's to rs2
            InstImp::nul_f3_f7("vx_wspawn", Opcode::Custom0, 1, 0, InstAction::SFU, || SFUType::WSPAWN as u32),
            // collect rs1[0] for then mask. divergent if mask not all 0 or 1. write divergence back. set tmc, push else mask to ipdom
            InstImp::nul_f3_f7("vx_split",  Opcode::Custom0, 2, 0, InstAction::SFU, || SFUType::SPLIT as u32),
            // rs1[0] indicates divergence from split. pop ipdom and set tmc if divergent
            InstImp::nul_f3_f7("vx_join",   Opcode::Custom0, 3, 0, InstAction::SFU, || SFUType::JOIN as u32),
            // rs1 indicates barrier id, rs2 indicates num warps participating in each core
            InstImp::nul_f3_f7("vx_bar",    Opcode::Custom0, 4, 0, InstAction::SFU, || SFUType::BAR as u32),
            // sets thread mask to current tmask & then_mask, same rules as split. if no lanes take branch, set mask to rs2.
            InstImp::nul_f3_f7("vx_pred",   Opcode::Custom0, 5, 0, InstAction::SFU, || SFUType::PRED as u32),
            InstImp::nul_f3_f7("vx_rast",   Opcode::Custom0, 0, 1, InstAction::SFU | InstAction::WRITE_REG, || todo!()),
        ];
        let sfu_insts = InstGroupVariant {
            insts: sfu_inst_imps,
            get_operands: |_| [],
        };

        let r3_inst_imps: Vec<InstImp<2>> = vec![
            InstImp::bin_f3_f7("add",  Opcode::Op, 0,  0, InstAction::WRITE_REG, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3_f7("sub",  Opcode::Op, 0, 32, InstAction::WRITE_REG, |a, b| { ((a as i32).wrapping_sub(b as i32)) as u32 }),
            InstImp::bin_f3_f7("sll",  Opcode::Op, 1,  0, InstAction::WRITE_REG, |a, b| { a << (b & 31) }),
            InstImp::bin_f3_f7("slt",  Opcode::Op, 2,  0, InstAction::WRITE_REG, |a, b| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            InstImp::bin_f3_f7("sltu", Opcode::Op, 3,  0, InstAction::WRITE_REG, |a, b| { if a < b { 1 } else { 0 } }),
            InstImp::bin_f3_f7("xor",  Opcode::Op, 4,  0, InstAction::WRITE_REG, |a, b| { a ^ b }),
            InstImp::bin_f3_f7("srl",  Opcode::Op, 5,  0, InstAction::WRITE_REG, |a, b| { a >> (b & 31) }),
            InstImp::bin_f3_f7("sra",  Opcode::Op, 5, 32, InstAction::WRITE_REG, |a, b| { ((a as i32) >> (b & 31)) as u32 }),
            InstImp::bin_f3_f7("or",   Opcode::Op, 6,  0, InstAction::WRITE_REG, |a, b| { a | b }),
            InstImp::bin_f3_f7("and",  Opcode::Op, 7,  0, InstAction::WRITE_REG, |a, b| { a & b }),

            InstImp::bin_f3_f7("mul",    Opcode::Op, 0, 1, InstAction::WRITE_REG, |a, b| { a.wrapping_mul(b) }),
            InstImp::bin_f3_f7("mulh",   Opcode::Op, 1, 1, InstAction::WRITE_REG, |a, b| { ((((a as i32) as i64).wrapping_mul((b as i32) as i64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("mulhsu", Opcode::Op, 2, 1, InstAction::WRITE_REG, |a, b| { ((((a as i32) as i64).wrapping_mul((b as u64) as i64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("mulhu",  Opcode::Op, 3, 1, InstAction::WRITE_REG, |a, b| { (((a as u64).wrapping_mul(b as u64)) >> 32) as u32 }),
            InstImp::bin_f3_f7("div",    Opcode::Op, 4, 1, InstAction::WRITE_REG, |a, b| { ((a as i32) / (Self::check_zero(b) as i32)) as u32 }),
            InstImp::bin_f3_f7("divu",   Opcode::Op, 5, 1, InstAction::WRITE_REG, |a, b| { a / Self::check_zero(b) }),
            InstImp::bin_f3_f7("rem",    Opcode::Op, 6, 1, InstAction::WRITE_REG, |a, b| { ((a as i32) % (Self::check_zero(b) as i32)) as u32 }),
            InstImp::bin_f3_f7("remu",   Opcode::Op, 7, 1, InstAction::WRITE_REG, |a, b| { a % Self::check_zero(b) }),
        ];
        let r3_insts = InstGroupVariant {
            insts: r3_inst_imps,
            get_operands: |req| [req.rs1, req.rs2],
        };

        let r4_inst_imps: Vec<InstImp<3>> = vec![
            InstImp::ter_f3_f2("vx_tex",  Opcode::Custom1, 0, 0, InstAction::WRITE_REG, |a, b, c| { todo!() }),
            InstImp::ter_f3_f2("vx_cmov", Opcode::Custom1, 1, 0, InstAction::WRITE_REG, |a, b, c| { todo!() }),
            InstImp::ter_f3_f2("vx_rop",  Opcode::Custom1, 1, 1, InstAction::NONE,      |a, b, c| { todo!() }),
        ];
        let r4_insts = InstGroupVariant {
            insts: r4_inst_imps,
            get_operands: |req| [req.rs1, req.rs2, req.rs3],
        };

        let i2_inst_imps: Vec<InstImp<2>> = vec![
            InstImp::bin_f3("lb",  Opcode::Load, 0, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("lh",  Opcode::Load, 1, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("lw",  Opcode::Load, 2, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("ld",  Opcode::Load, 3, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("lbu", Opcode::Load, 4, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("lhu", Opcode::Load, 5, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3("lwu", Opcode::Load, 6, InstAction::MEM_LOAD, |a, b| { a.wrapping_add(b) }),

            InstImp::bin_f3("fence",   Opcode::MiscMem, 0, InstAction::FENCE, |a, b| { 0 }),
            InstImp::bin_f3("fence.i", Opcode::MiscMem, 1, InstAction::FENCE, |a, b| { 1 }),

            InstImp::bin_f3   ("addi", Opcode::OpImm, 0,     InstAction::WRITE_REG, |a, b| { a.wrapping_add(b) }),
            InstImp::bin_f3_f7("slli", Opcode::OpImm, 1,  0, InstAction::WRITE_REG, |a, b| { a << (b & 31) }),
            InstImp::bin_f3   ("slti", Opcode::OpImm, 2,     InstAction::WRITE_REG, |a, b| { if (a as i32) < (b as i32) { 1 } else { 0 } }),
            InstImp::bin_f3  ("sltiu", Opcode::OpImm, 3,     InstAction::WRITE_REG, |a, b| { if a < b { 1 } else { 0 } }),
            InstImp::bin_f3   ("xori", Opcode::OpImm, 4,     InstAction::WRITE_REG, |a, b| { a ^ b }),
            InstImp::bin_f3_f7("srli", Opcode::OpImm, 5,  0, InstAction::WRITE_REG, |a, b| { a >> (b & 31) }),
            InstImp::bin_f3_f7("srai", Opcode::OpImm, 5, 32, InstAction::WRITE_REG, |a, b| { ((a as i32) >> (b & 31)) as u32 }),
            InstImp::bin_f3    ("ori", Opcode::OpImm, 6,     InstAction::WRITE_REG, |a, b| { a | b }),
            InstImp::bin_f3   ("andi", Opcode::OpImm, 7,     InstAction::WRITE_REG, |a, b| { a & b }),

            InstImp::bin_f3("jalr", Opcode::Jalr, 0, InstAction::SET_ABS_PC | InstAction::LINK, |a, b| { a.wrapping_add(b) }),
        ];
        let i2_insts = InstGroupVariant {
            insts: i2_inst_imps,
            get_operands: |req| [req.rs1, req.imm32 as u32],
        };

        // does not return anything
        let s_inst_imps: Vec<InstImp<2>> = vec![
            InstImp::bin_f3("sb", Opcode::Store, 0, InstAction::MEM_STORE, |a, imm| { a.wrapping_add(imm) }),
            InstImp::bin_f3("sh", Opcode::Store, 1, InstAction::MEM_STORE, |a, imm| { a.wrapping_add(imm) }),
            InstImp::bin_f3("sw", Opcode::Store, 2, InstAction::MEM_STORE, |a, imm| { a.wrapping_add(imm) }),
            /* InstImp::bin_f3("sd", Opcode::Store, 3, InstAction::MEM_STORE, |a, imm| { a + imm }), */
        ];
        let s_insts = InstGroupVariant {
            insts: s_inst_imps,
            get_operands: |req| [req.rs1, req.imm24 as u32],
        };

        // binary op returns branch offset if taken, 0 if not
        let sb_inst_imps: Vec<InstImp<3>> = vec![
            InstImp::ter_f3("beq",  Opcode::Branch, 0, InstAction::SET_REL_PC, |a, b, imm| { if a == b { imm } else { 0 }  }),
            InstImp::ter_f3("bne",  Opcode::Branch, 1, InstAction::SET_REL_PC, |a, b, imm| { if a != b { imm } else { 0 }  }),
            InstImp::ter_f3("blt",  Opcode::Branch, 4, InstAction::SET_REL_PC, |a, b, imm| { if (a as i32) < (b as i32) { imm } else { 0 }  }),
            InstImp::ter_f3("bge",  Opcode::Branch, 5, InstAction::SET_REL_PC, |a, b, imm| { if (a as i32) >= (b as i32) { imm } else { 0 }  }),
            InstImp::ter_f3("bltu", Opcode::Branch, 6, InstAction::SET_REL_PC, |a, b, imm| { if a < b { imm } else { 0 }  }),
            InstImp::ter_f3("bgeu", Opcode::Branch, 7, InstAction::SET_REL_PC, |a, b, imm| { if a >= b { imm } else { 0 }  }),
        ];
        let sb_insts = InstGroupVariant {
            insts: sb_inst_imps,
            get_operands: |req| [req.rs1, req.rs2, req.imm24 as u32],
        };

        let pcrel_inst_imps: Vec<InstImp<2>> = vec![
            InstImp::bin("auipc", Opcode::Auipc, InstAction::WRITE_REG, |a, b| { a.wrapping_add(b) }),
            InstImp::bin("jal", Opcode::Jal, InstAction::SET_ABS_PC| InstAction::LINK, |a, b| { a.wrapping_add(b) }),
        ];
        let pcrel_insts = InstGroupVariant {
            insts: pcrel_inst_imps,
            get_operands: |req| [req.pc, req.imm32 as u32],
        };

        let lui_inst_imp: Vec<InstImp<1>> = vec![
            InstImp::una("lui", Opcode::Lui, InstAction::WRITE_REG, |a| { a }),
        ];
        let lui_inst = InstGroupVariant {
            insts: lui_inst_imp,
            get_operands: |req| [(req.imm32 as u32) << 12],
        };

        vec![
            Box::new(InstGroup::Nullary(sfu_insts)),
            Box::new(InstGroup::Unary(lui_inst)),
            Box::new(InstGroup::Binary(r3_insts)),
            Box::new(InstGroup::Binary(i2_insts)),
            Box::new(InstGroup::Binary(s_insts)),
            Box::new(InstGroup::Binary(pcrel_insts)),
            Box::new(InstGroup::Ternary(r4_insts)),
            Box::new(InstGroup::Ternary(sb_insts)),
        ]
    }
}
