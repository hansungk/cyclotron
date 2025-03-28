extern crate num;

use crate::base::behavior::*;
use crate::base::module::{module, ModuleBase, IsModule};
use crate::muon::config::MuonConfig;
use crate::utils::*;
use std::fmt::Formatter;
use std::sync::Arc;

#[derive(Debug, Default, Copy, Clone)]
pub struct DecodedInst {
    pub opcode: u16,
    pub rd: u8,
    pub f3: u8,
    pub rs1: u32,
    pub rs2: u32,
    pub rs3: u32,
    pub rs4: u32,
    pub f7: u8,
    pub imm32: i32,
    pub imm24: i32,
    pub imm8: i32,
    pub rs2_addr: u8,
    pub pc: u32,
    pub raw: u64,
}

impl std::fmt::Display for DecodedInst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        
        write!(
            f,
            "inst {:#010x} [ op: 0x{:x}, f3: {}, f7: {}, rs1: 0x{:08x}, rs2: 0x{:08x} ]",
            self.raw, self.opcode, self.f3, self.f7, self.rs1, self.rs2
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

#[derive(Debug, Default)]
pub struct RegFile {
    base: ModuleBase<RegFileState, MuonConfig>,
}

// TODO: implement timing behavior for the regfile
impl ModuleBehaviors for RegFile {
    fn tick_one(&mut self) {}
    fn reset(&mut self) {
        self.base.state.gpr.fill(0u32);
        let config = self.conf();
        let gtid = config.num_warps * config.lane_config.core_id
            + config.num_lanes * config.lane_config.warp_id
            + config.lane_config.lane_id;
        self.base.state.gpr[2] = 0xffff0000u32 - (0x100000u32 * gtid as u32); // sp
    }
}

module!(RegFile, RegFileState, MuonConfig,
);

impl RegFile {
    pub fn new(config: Arc<MuonConfig>) -> RegFile {
        let mut me = RegFile::default();
        me.init_conf(config);
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
    pub fn decode(&self, inst_data: [u8; 8], pc: u32, rf: &RegFile) -> DecodedInst {
        let inst = u64::from_le_bytes(inst_data);

        let _pred: u8 = inst.sel(63, 60) as u8;
        let rs1_addr: u8 = inst.sel(27, 20) as u8;
        let rs2_addr: u8 = inst.sel(35, 28) as u8;
        let rs3_addr: u8 = inst.sel(43, 36) as u8;
        let rs4_addr: u8 = inst.sel(51, 44) as u8;

        let imm8: i32 = sign_ext::<8>(rs1_addr as u32);
        let imm24: i32 = sign_ext::<24>(inst.sel(59, 36) as u32);
        let uimm32: u32 = (inst.sel(59, 36) as u32) | ((inst.sel(35, 28) as u32) << 24);

        let _imm12_1: i32 = sign_ext::<12>(inst.sel(47, 36) as u32);
        let _imm12_2: i32 = sign_ext::<12>(inst.sel(59, 48) as u32);

        DecodedInst {
            opcode: inst.sel(8, 0) as u16,
            rd: inst.sel(16, 9) as u8,
            f3: inst.sel(19, 17) as u8,
            rs1: rf.read_gpr(rs1_addr),
            rs2: rf.read_gpr(rs2_addr),
            rs3: rf.read_gpr(rs3_addr),
            rs4: rf.read_gpr(rs4_addr),
            f7: inst.sel(58, 52) as u8,
            imm32: uimm32 as i32,
            imm24,
            imm8,
            rs2_addr,
            pc,
            raw: inst,
        }
    }
}

const fn bit_mask(msb: u64, lsb: u64) -> u64 {
    let len = msb - lsb + 1;
    let offset = lsb;

    ((1 << len) - 1) << offset
}

pub const PRED_MASK: u64 = bit_mask(63, 60);
pub const OPCODE_MASK: u64 = bit_mask(8, 0);

pub const RD_MASK: u64 = bit_mask(16, 9);
pub const RS1_MASK: u64 = bit_mask(27, 20);
pub const RS2_MASK: u64 = bit_mask(35, 28);
pub const RS3_MASK: u64 = bit_mask(43, 36);
pub const RS4_MASK: u64 = bit_mask(51, 44);

pub const IMM8_MASK: u64 = RS1_MASK;
pub const IMM24_MASK: u64 = bit_mask(59, 36);
pub const UIMM32_MASK: u64 = IMM24_MASK | RS2_MASK; // actual value is swizzled
pub const IMM12_1_MASK: u64 = bit_mask(47, 36);
pub const IMM12_2_MASK: u64 = bit_mask(59, 48);

pub const F3_MASK: u64 = bit_mask(19, 17);
pub const F7_MASK: u64 = bit_mask(58, 52);

#[macro_export]
macro_rules! make_bitpat {
    ($($a:tt => $b:expr),+) => {
        {
            #[allow(unused_imports)]
            use $crate::muon::decode::{
                PRED_MASK,
                OPCODE_MASK,
        
                RD_MASK,
                RS1_MASK,
                RS2_MASK,
                RS3_MASK,
                RS4_MASK,
        
                IMM8_MASK,
                IMM24_MASK,
                UIMM32_MASK,
                IMM12_1_MASK,
                IMM12_2_MASK,
        
                F3_MASK,
                F7_MASK
            };

            make_bitpat!(DUMMY, $($a => $b),+)
        }
    };
    

    (DUMMY, $a:tt => $b:expr) => {
        {
            let mask = $crate::muon::decode::$a;
            let pattern = $b << u64::trailing_zeros(mask);

            (mask, pattern)
        }
    };

    (DUMMY, $a:tt => $b:expr, $($c:tt => $d:expr),+) => {
        {
            let (inner_mask, inner_pattern) = make_bitpat!(DUMMY, $($c => $d),+);

            let mask = $crate::muon::decode::$a;
            let pattern = $b << u64::trailing_zeros(mask);

            (inner_mask | mask, inner_pattern | pattern)
        }
    };
}