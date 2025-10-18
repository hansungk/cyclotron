extern crate num;

use crate::base::behavior::*;
use crate::base::module::{module, ModuleBase, IsModule};
use crate::muon::config::MuonConfig;
use crate::utils::*;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

#[derive(Debug, Default, Copy, Clone)]
pub struct DecodedInst {
    pub opcode: u8,
    pub opext: u8,
    pub rd_addr: u8,
    pub f3: u8,
    pub rs1_addr: u8,
    pub rs2_addr: u8,
    pub rs3_addr: u8,
    pub rs4_addr: u8,
    pub f7: u8,
    pub imm32: u32,
    pub imm24: i32,
    pub csr_imm: u8,
    pub pc: u32,
    pub raw: u64,
}

/// Instruction bundle after operand collection, i.e. rs_addr -> rs_data
pub struct IssuedInst {
    pub opcode: u8,
    pub opext: u8,
    pub rd_addr: u8,
    pub f3: u8,
    pub rs1_addr: u8,
    pub rs2_addr: u8,
    pub rs3_addr: u8,
    pub rs4_addr: u8,
    pub rs1_data: Vec<Option<u32>>,
    pub rs2_data: Vec<Option<u32>>,
    pub rs3_data: Vec<Option<u32>>,
    pub rs4_data: Vec<Option<u32>>,
    pub f7: u8,
    pub imm32: u32,
    pub imm24: i32,
    pub csr_imm: u8,
    pub pc: u32,
    pub raw: u64,
}

impl Debug for IssuedInst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedInst")
            .field("op", &format_args!("0x{:x}", self.opcode))
            .field("opext", &self.opext)
            .field("rd", &self.rd_addr)
            .field("f3", &self.f3)
            // .field("rs1", &self.rs1_addr)
            // .field("rs2", &self.rs2_addr)
            // .field("rs3", &self.rs3_addr)
            // .field("rs4", &self.rs4_addr)
            // .field("rs1_data", &self.rs1_data)
            // .field("rs2_data", &self.rs2_data)
            // .field("rs3_data", &self.rs3_data)
            // .field("rs4_data", &self.rs4_data)
            .field("f7", &self.f7)
            .field("imm32", &format_args!("0x{:x}", self.imm32))
            .field("imm24", &self.imm24)
            .field("csr_imm", &self.csr_imm)
            .field("pc", &format_args!("{:X}", self.pc))
            .field("raw", &format_args!("0x{:x}", self.raw))
            .finish()
    }
}

impl std::fmt::Display for IssuedInst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pc {:#010x} inst {:#010x} [ op: 0x{:x}, f3: {}, f7: {}, rd=x{}, rs1=x{}, x{}, x{} ]",
            self.pc, self.raw, self.opcode, self.f3, self.f7,
            self.rd_addr, self.rs1_addr, self.rs2_addr, self.rs3_addr
        )
    }
}

// per-warp
#[derive(Debug, Clone, Copy)]
pub struct InstBufEntry {
    pub inst: DecodedInst,
    pub tmask: u32,
}
pub struct InstBuf(pub Vec<Option<InstBufEntry>>);

impl std::fmt::Display for DecodedInst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "inst {:#010x} [ op: 0x{:x}, f3: {}, f7: {}, rd=x{}, rs1=x{}, x{}, x{}, rs4=x{} ]",
            self.raw, self.opcode, self.f3, self.f7,
            self.rd_addr, self.rs1_addr, self.rs2_addr, self.rs3_addr, self.rs4_addr
        )
    }
}

pub fn sign_ext<const W: u8>(from: u32) -> i32 {
    assert!(
        W <= 32,
        "cannot extend a two's complement number that is more than 32 bits"
    );
    ((from << (32 - W)) as i32) >> (32 - W)
}

#[derive(Debug)]
pub struct RegFileState {
    gpr: [u32; 128],
}

impl Default for RegFileState {
    fn default() -> Self {
        Self {
            gpr: [0u32; 128],
        }
    }
}

/// Register file for a single SIMT lane.
#[derive(Debug, Default)]
pub struct RegFile {
    base: ModuleBase<RegFileState, MuonConfig>,
    lane_id: usize,
}

impl ModuleBehaviors for RegFile {
    fn tick_one(&mut self) {}
    fn reset(&mut self) {
        self.base.state.gpr.fill(0u32);
        let gtid = self.conf().num_lanes * self.conf().num_warps *
            self.conf().lane_config.core_id +
            self.conf().num_lanes * self.conf().lane_config.warp_id +
            self.lane_id;
        self.base.state.gpr[2] = 0xffff0000u32 - (0x100000u32 * gtid as u32); // sp
    }
}

module!(RegFile, RegFileState, MuonConfig,
);

impl RegFile {
    pub fn new(config: Arc<MuonConfig>, lid: usize) -> RegFile {
        let mut me = RegFile::default();
        me.init_conf(config);
        me.lane_id = lid;
        me
    }

    pub fn read_gpr(&self, addr: u8) -> u32 {
        if addr == 0 {
            0u32
        } else {
            self.base.state.gpr[(addr & 0x7f) as usize]
        }
    }

    pub fn write_gpr(&mut self, addr: u8, data: u32) {
        assert!(addr < 128, "invalid gpr value {}", addr);
        if addr > 0 {
            self.base.state.gpr[addr as usize] = data;
        }
    }
}

#[derive(Debug)]
pub struct DecodeUnit;

impl DecodeUnit {
    pub fn decode(inst: u64, pc: u32) -> DecodedInst {
        let _pred: u8 = inst.sel(63, 60) as u8;
        let rs1_addr: u8 = inst.sel(27, 20) as u8;
        let rs2_addr: u8 = inst.sel(35, 28) as u8;
        let rs3_addr: u8 = inst.sel(43, 36) as u8;
        let rs4_addr: u8 = inst.sel(51, 44) as u8;

        let csr_imm: u8 = rs1_addr; // duplicate, because rs1 can be renamed

        let imm24: i32 = sign_ext::<24>(inst.sel(59, 36) as u32);
        let uimm32: u32 = (inst.sel(59, 36) as u32) | ((inst.sel(35, 28) as u32) << 24);

        let _imm12_1: i32 = sign_ext::<12>(inst.sel(47, 36) as u32);
        let _imm12_2: i32 = sign_ext::<12>(inst.sel(59, 48) as u32);

        DecodedInst {
            opcode: inst.sel(6, 0) as u8,
            opext: inst.sel(8, 7) as u8,
            rd_addr: inst.sel(16, 9) as u8,
            f3: inst.sel(19, 17) as u8,
            rs1_addr,
            rs2_addr,
            rs3_addr,
            rs4_addr,
            f7: inst.sel(58, 52) as u8,
            imm32: uimm32,
            imm24,
            csr_imm,
            pc,
            raw: inst,
        }
    }
}
