use crate::base::behavior::*;
use crate::base::mem::HasMemory;
use crate::base::module::{module, IsModule, ModuleBase};
use crate::info;
use crate::muon::config::MuonConfig;
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, IssuedInst, MicroOp, RegFile};
use crate::muon::execute::ExecuteUnit;
use crate::muon::scheduler::{Schedule, Scheduler, SchedulerWriteback};
use crate::neutrino::neutrino::Neutrino;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use std::fmt::Debug;
use std::fmt::{Display, Formatter};
use std::iter::zip;
use std::ops::DerefMut;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, RwLock};

#[derive(Debug, Default)]
pub struct WarpState {
    pub reg_file: Vec<RegFile>,
    pub csr_file: Vec<CSRFile>,
}

pub struct Warp {
    pub base: ModuleBase<WarpState, MuonConfig>,
    pub wid: usize,
    logger: Arc<Logger>,
    gmem: Arc<RwLock<FlatMemory>>,
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

module!(Warp, WarpState, MuonConfig,);

/// Writeback result from the execute stage modulo memory load/stores,
/// which will be handled in the mem stage.
#[derive(Debug)]
pub struct ExWriteback {
    pub inst: IssuedInst,
    pub tmask: u32,
    pub rd_addr: u8,
    pub rd_data: Vec<Option<u32>>,
    pub mem_req: Vec<Option<MemRequest>>,
    pub sched_wb: SchedulerWriteback,
}

#[derive(Clone, Debug)]
pub struct MemRequest {
    pub addr: u32,
    pub data: Option<u32>,
    pub size: u32,
    pub is_sext: bool,
    pub is_store: bool,
    pub is_smem: bool,
}

#[derive(Clone, Debug)]
pub struct MemResponse {
    pub data: Option<[u8; 4]>,
    pub is_sext: bool,
}

#[derive(Debug)]
pub struct Writeback {
    pub inst: IssuedInst,
    pub tmask: u32,
    pub rd_addr: u8,
    pub rd_data: Vec<Option<u32>>,
    pub sched_wb: SchedulerWriteback,
}

#[derive(Debug, Default, Clone)]
pub struct ExecErr {
    pub pc: u32,
    pub warp_id: usize,
    pub message: Option<String>,
}

// TODO: make ExecErr a proper error type by providing impls
// impl Display for ExecErr
// impl Error for ExecErr

impl Writeback {
    pub fn rd_data_str(&self) -> String {
        let lanes: Vec<String> = self
            .rd_data
            .iter()
            .map(|lrd| match lrd {
                Some(value) => format!("0x{:x}", value),
                None => ".".to_string(),
            })
            .collect();
        format!("[{}]", lanes.join(","))
    }

    pub fn num_rd_data(&self) -> usize {
        self.rd_data.iter().map(|lrd| lrd.is_some()).count()
    }
}

impl Display for Writeback {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Writeback {{ inst: {}, tmask: {:b}, rd_addr: {}, rd_data: {:?}, sched_wb: {{ pc: {:?} }} }}",
            self.inst, self.tmask, self.rd_addr, self.rd_data, self.sched_wb.pc
        )
    }
}

impl Warp {
    pub fn new(
        config: Arc<MuonConfig>,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
    ) -> Warp {
        let num_lanes = config.num_lanes;
        let mut me = Warp {
            base: ModuleBase {
                state: WarpState {
                    reg_file: (0..num_lanes)
                        .map(|i| RegFile::new(config.clone(), i))
                        .collect(),
                    csr_file: (0..num_lanes)
                        .map(|i| CSRFile::new(config.clone(), i))
                        .collect(),
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
        format!(
            "core {} warp {}",
            self.conf().lane_config.core_id,
            self.conf().lane_config.warp_id
        )
    }

    /// Returns un-decoded instruction bits at a given PC.
    pub fn fetch(&self, pc: u32) -> u64 {
        let inst_bytes = self
            .gmem
            .write()
            .expect("gmem poisoned")
            .read_n::<8>(pc as usize)
            .expect("failed to fetch instruction");
        u64::from_le_bytes(inst_bytes)
    }

    pub fn frontend(&mut self, schedule: Schedule) -> MicroOp {
        // fetch
        let inst = self.fetch(schedule.pc);
        for c in self.state_mut().csr_file.iter_mut() {
            c.emu_access(0xcc3, schedule.active_warps);
            c.emu_access(0xcc4, schedule.mask);
        }
        self.frontend_nofetch(schedule, inst)
    }

    // Used for co-sim where RTL fetches the instruction.
    pub fn frontend_nofetch(&mut self, schedule: Schedule, inst: u64) -> MicroOp {
        // decode
        let inst = DecodeUnit::decode(inst, schedule.pc);
        let tmask = schedule.mask;
        MicroOp { inst, tmask }
    }

    pub fn backend(
        &mut self,
        uop: MicroOp,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
        smem: &mut FlatMemory,
    ) -> Result<Writeback, ExecErr> {
        let pc = uop.inst.pc;
        let tmask = uop.tmask;

        assert!(
            scheduler.state().thread_masks[self.wid] == tmask,
            "scheduler tmask was not set correctly"
        );
        scheduler.state_mut().thread_masks[self.wid] = tmask;

        // operand collection
        let issued = self.collect(&uop);

        // execute
        let writeback = catch_unwind(AssertUnwindSafe(|| {
            let ex_wb = self.execute_nomem(issued, tmask, scheduler, neutrino);
            self.mem(&ex_wb, smem, None)
        }));

        // writeback
        match writeback {
            Ok(writeback) => {
                self.writeback(&writeback);

                info!(
                    self.logger,
                    "@t={} [{}] PC=0x{:08x}, rd={:3}, data=[{} lanes valid]",
                    self.base.cycle,
                    self.name(),
                    pc,
                    writeback.rd_addr,
                    writeback.num_rd_data()
                );

                Ok(writeback)
            }
            Err(payload) => {
                let message = payload.downcast::<String>().ok().map(|s| *s);
                Err(ExecErr {
                    pc,
                    warp_id: self.wid,
                    message,
                })
            }
        }
    }

    /// COLL/EX/MEM req stage before mem request is served.
    /// Used for co-sim with decoupled memory.
    pub fn backend_beforemem(
        &mut self,
        uop: MicroOp,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
    ) -> Result<ExWriteback, ExecErr> {
        let pc = uop.inst.pc;
        let tmask = uop.tmask;

        assert!(
            scheduler.state().thread_masks[self.wid] == tmask,
            "scheduler tmask was not set correctly"
        );

        // operand collection
        let issued = self.collect(&uop);

        // execute
        let ex_writeback = catch_unwind(AssertUnwindSafe(|| {
            self.execute_nomem(issued, tmask, scheduler, neutrino)
        }));

        // writeback
        match ex_writeback {
            Ok(wb) => Ok(wb),
            Err(payload) => {
                let message = payload.downcast::<String>().ok().map(|s| *s);
                Err(ExecErr {
                    pc,
                    warp_id: self.wid,
                    message,
                })
            }
        }
    }

    /// Collect source operand values from the regfile.
    pub fn collect(&mut self, uop: &MicroOp) -> IssuedInst {
        let rf = self.base.state.reg_file.as_slice();
        ExecuteUnit::collect(&uop, rf)
    }

    /// Execute a single-warp issued instruction and return a writeback bundle.
    pub fn execute(
        &mut self,
        issued: IssuedInst,
        tmask: u32,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
        smem: &mut FlatMemory,
    ) -> Writeback {
        let ex_wb = self.execute_nomem(issued, tmask, scheduler, neutrino);
        self.mem(&ex_wb, smem, None)
    }

    /// Execute stage, but only generates mem requests without serving them with the internal
    /// gmem/smem.
    /// Used for co-sim with decoupled memory.
    pub fn execute_nomem(
        &mut self,
        issued: IssuedInst,
        tmask: u32,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
    ) -> ExWriteback {
        let core_id = self.conf().lane_config.core_id;
        let rf = self.base.state.reg_file.as_mut_slice();
        let csrf = self.base.state.csr_file.as_mut_slice();

        // ALU/FPU/AddrGen stage
        ExecuteUnit::execute(
            issued, core_id, self.wid, tmask, rf, csrf, scheduler, neutrino,
        )
    }

    /// Memory stage; serve the memory requests produced in EX.
    /// If `external_mem_resp` is given, handle mem requests with an explicit memory response supplied by an
    /// external memory model e.g. RTL, instead of using internal gmem/smem.
    /// TODO: handle RTL response
    pub fn mem(
        &mut self,
        ex_writeback: &ExWriteback,
        smem: &mut FlatMemory,
        external_mem_resp: Option<&Vec<Option<MemResponse>>>,
    ) -> Writeback {
        let external_mem = external_mem_resp.is_some();

        let rd_mem = ex_writeback
            .mem_req
            .iter()
            .enumerate()
            .map(|(i, oreq)| match oreq {
                Some(req) => {
                    let resp = if external_mem {
                        external_mem_resp.unwrap()[i]
                            .as_ref()
                            .expect("missing mem response for a lane")
                    } else {
                        &self.mem_response(req, smem)
                    };
                    ExecuteUnit::mem_writeback(req, resp)
                }
                None => None,
            });

        // consolidate rd's from EX and MEM
        let rd_merged = zip(&ex_writeback.rd_data, rd_mem)
            .map(|(lrd_ex, lrd_mem)| {
                assert!(
                    lrd_mem.is_none() || lrd_ex.is_none(),
                    "mem req generated for a lane that already has an EX rd writeback"
                );
                lrd_mem.or(*lrd_ex)
            })
            .collect::<Vec<_>>();

        Writeback {
            inst: ex_writeback.inst.clone(),
            tmask: ex_writeback.tmask,
            rd_addr: ex_writeback.rd_addr,
            rd_data: rd_merged,
            sched_wb: ex_writeback.sched_wb,
        }
    }

    /// Handle a per-lane memory request and generate a MemResponse.
    pub fn mem_response(&mut self, mem_req: &MemRequest, smem: &mut FlatMemory) -> MemResponse {
        let mut gmem = self.gmem.write().expect("lock poisoned");
        let mem = if mem_req.is_smem {
            smem
        } else {
            gmem.deref_mut()
        };

        let addr = mem_req.addr;
        let addr_aligned = addr >> 2 << 2;
        let size = mem_req.size;

        if mem_req.is_store {
            let store_data = mem_req.data.expect("store req missing a data field");
            let store_data_bytes = store_data.to_le_bytes();

            match size {
                1 => mem.write(addr as usize, &store_data_bytes[0..1]), // store byte
                2 => mem.write(addr as usize, &store_data_bytes[0..2]), // store half
                4 => mem.write(addr as usize, &store_data_bytes[0..4]), // store word
                _ => panic!("unimplemented store size"),
            }
            .expect("store failed");

            MemResponse {
                data: None,
                is_sext: mem_req.is_sext, // pass-through
            }
        } else {
            let load_data = mem.read_n::<4>(addr_aligned as usize).expect("load failed");

            // byte-masking will be handled in Execute::mem
            MemResponse {
                data: Some(load_data),
                is_sext: mem_req.is_sext, // pass-through
            }
        }
    }

    pub fn writeback(&mut self, wb: &Writeback) {
        let rf = self.base.state.reg_file.as_mut_slice();

        // note: scheduler writeback is done in-place
        let rd_addr = wb.rd_addr;
        for (lrf, ldata) in zip(rf, &wb.rd_data) {
            ldata.map(|data| lrf.write_gpr(rd_addr, data));
        }
    }

    pub fn set_block_threads_bp(
        &mut self,
        block_idx: (u32, u32, u32),
        thread_idxs: &Vec<(u32, u32, u32)>,
        bp: u32,
    ) {
        assert!(thread_idxs.len() <= self.base.state.csr_file.len());
        assert!(thread_idxs.len() > 0);
        for (csr_file, thread_idx) in zip(self.base.state.csr_file.iter_mut(), thread_idxs.iter()) {
            csr_file.set_block_thread_bp(block_idx, *thread_idx, bp);
        }
    }
}
