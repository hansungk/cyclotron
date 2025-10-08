use crate::base::behavior::*;
use crate::base::mem::HasMemory;
use crate::base::module::{module, IsModule, ModuleBase};
use crate::muon::config::MuonConfig;
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, DecodedInst, InstBufEntry, RegFile};
use crate::muon::execute::ExecuteUnit;
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::sim::log::Logger;
use crate::info;
use crate::sim::toy_mem::ToyMemory;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, RwLock};
use crate::neutrino::neutrino::Neutrino;

#[derive(Debug, Default)]
pub struct WarpState {
    pub reg_file: Vec<RegFile>,
    pub csr_file: Vec<CSRFile>,
}

pub struct Warp {
    base: ModuleBase<WarpState, MuonConfig>,
    pub wid: usize,
    logger: Arc<Logger>,
    gmem: Arc<RwLock<ToyMemory>>,
}

impl ModuleBehaviors for Warp {
    fn tick_one(&mut self) {
        self.base().cycle += 1;
    }

    fn reset(&mut self) {
        self.state_mut().reg_file.iter_mut().for_each(|x| x.reset());
        self.state_mut().csr_file.iter_mut().for_each(|x| x.reset());
    }
}

module!(Warp, WarpState, MuonConfig,
);

#[derive(Debug)]
pub struct Writeback {
    pub inst: DecodedInst,
    pub tmask: u32,
    pub rd_addr: u8,
    pub rd_data: Vec<Option<u32>>,
}

#[derive(Debug, Default, Clone)]
pub struct ExecErr {
    pub pc: u32,
    pub warp_id: usize,
    pub message: Option<String>,
}

impl Default for Writeback {
    fn default() -> Self {
        Writeback {
            inst: Default::default(),
            tmask: 0,
            rd_addr: 0,
            rd_data: Vec::new(),
        }
    }
}

impl Writeback {
    pub fn rd_data_str(&self) -> String {
        let lanes: Vec<String> = self.rd_data.iter().map(|lrd| {
            match lrd {
                Some(value) => format!("0x{:x}", value),
                None => ".".to_string(),
            }
        }).collect();
        format!("[{}]", lanes.join(","))
    }

    pub fn num_rd_data(&self) -> usize {
        self.rd_data.iter().map(|lrd| lrd.is_some()).count()
    }
}

impl Warp {
    pub fn new(config: Arc<MuonConfig>, logger: &Arc<Logger>, gmem: Arc<RwLock<ToyMemory>>) -> Warp {
        let num_lanes = config.num_lanes;
        let mut me = Warp {
            base: ModuleBase {
                state: WarpState {
                    reg_file: (0..num_lanes).map(|i| RegFile::new(config.clone(), i)).collect(),
                    csr_file: (0..num_lanes).map(|i| CSRFile::new(config.clone(), i)).collect(),
                },
                ..ModuleBase::default()
            },
            wid: config.lane_config.warp_id,
            logger: logger.clone(),
            gmem,
        };
        me.init_conf(config);
        me
    }

    fn name(&self) -> String {
        format!("core {} warp {}", self.conf().lane_config.core_id, self.conf().lane_config.warp_id)
    }

    /// Returns un-decoded instruction bits at a given PC.
    pub fn fetch(&self, pc: u32) -> u64 {
        let inst_bytes = *self.gmem.write()
            .expect("gmem poisoned")
            .read::<8>(pc as usize)
            .expect("failed to fetch instruction");
        u64::from_le_bytes(inst_bytes)
    }

    pub fn frontend(&mut self, schedule: Schedule) -> InstBufEntry {
        // fetch
        let inst = self.fetch(schedule.pc);
        self.state_mut().csr_file.iter_mut().for_each(|c| {
            c.emu_access(0xcc3, schedule.active_warps);
            c.emu_access(0xcc4, schedule.mask);
        });

        // decode
        let inst = DecodeUnit::decode(inst, schedule.pc);
        let tmask = schedule.mask;
        InstBufEntry { inst, tmask }
    }

    pub fn backend(&mut self, ibuf: &InstBufEntry, scheduler: &mut Scheduler, neutrino: &mut Neutrino) -> Result<Writeback, ExecErr> {
        let inst = ibuf.inst;
        let tmask = ibuf.tmask;
        let core_id = self.conf().lane_config.core_id;
        let mut rf = self.base.state.reg_file.iter_mut().collect::<Vec<_>>();
        let mut csrf = self.base.state.csr_file.iter_mut().collect::<Vec<_>>();

        let writeback = catch_unwind(AssertUnwindSafe(|| {
            ExecuteUnit::execute(
                inst,
                core_id,
                self.wid,
                tmask,
                &mut rf,
                &mut csrf,
                scheduler,
                neutrino,
                &mut self.gmem,
            )
        }));

        match writeback {
            Ok(writeback) => {
                Self::writeback(&writeback, &mut rf);

                info!(self.logger, "@t={} [{}] PC=0x{:08x}, rd={:3}, data=[{} lanes valid]",
                    self.base.cycle,
                    self.name(),
                    inst.pc,
                    writeback.rd_addr,
                    writeback.num_rd_data()
                );

                Ok(writeback)
            }
            Err(payload) => {
                let message = payload.downcast::<String>().ok().map(|s| *s);
                Err(ExecErr {
                    pc: inst.pc,
                    warp_id: self.wid,
                    message,
                })
            }
        }
    }

    /// Fast-path that fuses frontend/backend for every warp instead of two-stage schedule/ibuf
    /// iteration.
    pub fn execute(&mut self, schedule: Schedule,
                   scheduler: &mut Scheduler, neutrino: &mut Neutrino) -> Result<(), ExecErr> {
        let ibuf = self.frontend(schedule.clone());
        self.backend(&ibuf, scheduler, neutrino).map(|_| ())
    }

    pub fn writeback(wb: &Writeback, rf: &mut Vec<&mut RegFile>) {
        let rd_addr = wb.rd_addr;
        rf.iter_mut().zip(wb.rd_data.iter()).for_each(|(lrf, ldata)| {
            ldata.map(|data| lrf.write_gpr(rd_addr, data));
        });
    }

    pub fn set_block_threads(&mut self, block_idx: (u32, u32, u32), thread_idxs: &Vec<(u32, u32, u32)>) {
        assert_eq!(self.base.state.csr_file.len(), thread_idxs.len());
        self.base.state.csr_file.iter_mut().zip(thread_idxs.iter()).for_each(|(csr_file, thread_idx)| {
            csr_file.set_block_thread(block_idx, *thread_idx);
        });
    }

}
