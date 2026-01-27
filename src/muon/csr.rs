use crate::base::behavior::*;
use crate::base::module::*;
use crate::muon::config::MuonConfig;
use crate::muon::execute::CSRType;
use log::info;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Default)]
pub struct CSRState {
    csr: HashMap<u32, u32>,
}

// this is instantiated per lane
#[derive(Debug, Default)]
pub struct CSRFile {
    base: ModuleBase<CSRState, MuonConfig>,
    lock: RwLock<()>,
    lane_id: usize,
    block_idx: (u32, u32, u32),
    thread_idx: (u32, u32, u32),
}

impl ModuleBehaviors for CSRFile {
    fn tick_one(&mut self) {
        // TODO: count cycles and stuff
    }

    fn reset(&mut self) {
        self.base.state.csr.clear();
    }
}

module!(CSRFile, CSRState, MuonConfig,);

macro_rules! get_ref_rw_match {
    ($self:expr, $variable:expr, [$( $addr:expr, $init:expr );* $(;)?]) => {
        match $variable {
            $( $addr => Some($self.base.state.csr.entry($addr).or_insert($init)), )*
            _ => None,
        }
    };
}

macro_rules! get_ro_match {
    ($variable:expr, [$( $addr:expr, $init:expr );* $(;)?]) => {
        match $variable {
            $( $addr => Some($init), )*
            _ => None,
        }
    };
}

impl CSRFile {
    pub fn new(config: Arc<MuonConfig>, lid: usize) -> Self {
        let mut csr = CSRFile::default();
        csr.lock = RwLock::new(());
        csr.init_conf(config);
        csr.lane_id = lid;
        csr
    }

    // these are constant values
    fn csr_ro_ref(&self, addr: u32) -> Option<u32> {
        let mhartid =
            self.conf().num_lanes * self.conf().num_warps * self.conf().lane_config.core_id
                + self.conf().num_lanes * self.conf().lane_config.warp_id
                + self.lane_id;
        get_ro_match!(addr, [
            0xf11, 0; // mvendorid
            0xf12, 0; // marchid
            0xf13, 0; // mimpid
            0x301, (1 << 30)
                | (0 <<  0) /* A - Atomic Instructions extension */
                | (0 <<  1) /* B - Tentatively reserved for Bit operations extension */
                | (0 <<  2) /* C - Compressed extension */
                | (0 <<  3) /* D - Double precsision floating-point extension */
                | (0 <<  4) /* E - RV32E base ISA */
                | (1 <<  5) /* F - Single precsision floating-point extension */
                | (0 <<  6) /* G - Additional standard extensions present */
                | (0 <<  7) /* H - Hypervisor mode implemented */
                | (1 <<  8) /* I - RV32I/64I/128I base ISA */
                | (0 <<  9) /* J - Reserved */
                | (0 << 10) /* K - Reserved */
                | (0 << 11) /* L - Tentatively reserved for Bit operations extension */
                | (1 << 12) /* M - Integer Multiply/Divide extension */
                | (0 << 13) /* N - User level interrupts supported */
                | (0 << 14) /* O - Reserved */
                | (0 << 15) /* P - Tentatively reserved for Packed-SIMD extension */
                | (0 << 16) /* Q - Quad-precision floating-point extension */
                | (0 << 17) /* R - Reserved */
                | (0 << 18) /* S - Supervisor mode implemented */
                | (0 << 19) /* T - Tentatively reserved for Transactional Memory extension */
                | (1 << 20) /* U - User mode implemented */
                | (0 << 21) /* V - Tentatively reserved for Vector extension */
                | (0 << 22) /* W - Reserved */
                | (1 << 23) /* X - Non-standard extensions present */
                | (0 << 24) /* Y - Reserved */
                | (0 << 25) /* Z - Reserved */
            ; // misa
            0x180, 0; // satp
            0x300, 0; // mstatus
            0x302, 0; // medeleg
            0x303, 0; // mideleg
            0x304, 0; // mie
            0x305, 0; // mtvec
            0x341, 0; // mepc
            0x3a0, 0; // pmpcf0
            0x3b0, 0; // pmpaddr0
            0xb01, 0; // mpm_reserved
            0xb81, 0; // mpm_reserved_h

            0xf14, mhartid as u32; // mhartid
            0xcc0, self.lane_id as u32; // thread_id
            0xcc1, self.conf().lane_config.warp_id as u32; // warp_id
            0xcc2, self.conf().lane_config.core_id as u32; // core_id
            0xfc0, self.conf().num_lanes as u32; // num_threads
            0xfc1, self.conf().num_warps as u32; // num_warps
            0xfc2, self.conf().num_cores as u32; // num_cores
            0xfc3, self.block_idx.0; // block_idx.x
            0xfc4, self.block_idx.1; // block_idx.y
            0xfc5, self.block_idx.2; // block_idx.z
            0xfc6, self.thread_idx.0; // thread_idx.x
            0xfc7, self.thread_idx.1; // thread_idx.y
            0xfc8, self.thread_idx.2; // thread_idx.z
        ])
    }

    // these can only be read by the user,
    // but the emulator can update them
    fn csr_rw_ref_emu(&mut self, addr: u32) -> Option<&mut u32> {
        let _lock = self.lock.write().expect("lock poisoned");
        get_ref_rw_match!(self, addr, [
            0xcc3, 0; // warp_mask
            0xcc4, 0; // thread_mask
            0xb00, 0; // mcycle
            0xb80, 0; // mcycle_h
            0xb02, 0; // minstret
            0xb82, 0; // minstret_h
        ])
    }

    // these are writable by user with an initial value
    fn csr_rw_ref_user(&mut self, addr: u32) -> Option<&mut u32> {
        let _lock = self.lock.write().expect("lock poisoned");
        get_ref_rw_match!(self, addr, [
            0xacc, 0; // cisc accelerator
            0x001, 0; // fflags
            0x002, 0; // frm
            0x003, 0; // fcsr
        ])
    }

    pub fn user_access(&mut self, addr: u32, value: u32, op: CSRType) -> u32 {
        if let Some(w) = self.csr_rw_ref_user(addr) {
            // writable
            let old_value = w.clone();
            match op {
                CSRType::RC | CSRType::RCI => *w &= !value,
                CSRType::RS | CSRType::RSI => *w |= value,
                CSRType::RW | CSRType::RWI => *w = value,
            }
            let new_value = *w;
            if (addr >= 0x001 && addr <= 0x003) && (new_value != old_value) {
                let old_fcsr = *self.csr_rw_ref_user(0x003).unwrap();
                // fcsr is a special case, we need to update fflags and frm
                if addr == 0x001 {
                    self.emu_access(0x003, (old_fcsr & 0xe0) | (new_value & 0x1f))
                } else if addr == 0x002 {
                    self.emu_access(0x003, (old_fcsr & 0x1f) | ((new_value & 0x7) << 5))
                } else {
                    self.emu_access(0x003, new_value & 0xff)
                }
            }
            old_value
        } else {
            // read-only
            self.csr_rw_ref_emu(addr)
                .map(|x| *x)
                .or(self.csr_ro_ref(addr))
                .expect(&format!("reading nonexistent csr {}", addr))
        }
    }

    pub fn emu_access(&mut self, addr: u32, value: u32) {
        if let Some(emu_ref) = self.csr_rw_ref_emu(addr) {
            *emu_ref = value
        } else {
            *self
                .csr_rw_ref_user(addr)
                .expect(&format!("setting nonexistent csr 0x{:x}", addr)) = value;
            if addr == 0x003 {
                info!("set fcsr to 0x{:x}", value);
                *self.csr_rw_ref_user(0x001).unwrap() = value & 0x1f; // fflags
                *self.csr_rw_ref_user(0x002).unwrap() = (value & 0xe0) >> 5; // frm
            }
        }
    }

    pub fn set_block_thread(&mut self, block_idx: (u32, u32, u32), thread_idx: (u32, u32, u32)) {
        self.block_idx = block_idx;
        self.thread_idx = thread_idx;
    }
}
