use crate::base::behavior::*;
use crate::base::mem::HasMemory;
use crate::base::module::{module, IsModule, ModuleBase};
use crate::info;
use crate::muon::config::MuonConfig;
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, IssuedInst, MicroOp, RegFile};
use crate::muon::execute::{ExecuteUnit, Opcode};
use crate::muon::gmem::CoreTimingModel;
use crate::muon::scheduler::{Schedule, Scheduler, SchedulerWriteback};
use crate::neutrino::neutrino::Neutrino;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use crate::timeflow::{GmemRequest, SmemRequest};
use crate::timeq::{module_now, Cycle};
use crate::utils::BitSlice;
use std::fmt::Debug;
use std::fmt::{Display, Formatter};
use std::iter::zip;
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
        timing_model: &mut CoreTimingModel,
        now: Cycle,
    ) -> Result<Option<Writeback>, ExecErr> {
        let decoded = uop.inst;
        let pc = decoded.pc;
        let tmask = uop.tmask;

        // TODO: verify that this step doesnt modify
        scheduler.state_mut().thread_masks[self.wid] = tmask;

        if decoded.opcode == Opcode::LOAD || decoded.opcode == Opcode::STORE {
            if decoded.opext == 1 {
                if self
                    .issue_smem_request(
                        decoded.opcode,
                        decoded.f3,
                        decoded.opcode == Opcode::LOAD,
                        decoded.rs1_addr,
                        decoded.imm32,
                        tmask,
                        scheduler,
                        timing_model,
                        now,
                    )
                    .is_err()
                {
                    return Ok(None);
                }
            } else if self
                .issue_gmem_request(
                    decoded.opcode,
                    decoded.opext,
                    decoded.f3,
                    decoded.rs1_addr,
                    decoded.imm32,
                    tmask,
                    scheduler,
                    timing_model,
                    now,
                )
                .is_err()
            {
                return Ok(None);
            }
        }

        if decoded.opcode == Opcode::MISC_MEM {
            let active_lanes = tmask.count_ones();
            if active_lanes > 0 {
                let mut flush_req = if decoded.f3 == 1 {
                    GmemRequest::new_flush_l0(self.wid, 1)
                } else {
                    GmemRequest::new_flush_l1(self.wid, 1)
                };
                if timing_model
                    .issue_gmem_request(now, self.wid, flush_req, scheduler)
                    .is_err()
                {
                    return Ok(None);
                }
            }
        }

        // operand collection
        let rf = self.base.state.reg_file.as_slice();
        let issued = ExecuteUnit::collect(&uop, rf);

        // execute
        let writeback = catch_unwind(AssertUnwindSafe(|| {
            self.execute(issued, tmask, scheduler, neutrino, smem)
        }));

        // writeback
        match writeback {
            Ok(writeback) => {
                let rf_mut = self.base.state.reg_file.as_mut_slice();
                Self::writeback(&writeback, rf_mut);

                info!(
                    self.logger,
                    "@t={} [{}] PC=0x{:08x}, rd={:3}, data=[{} lanes valid]",
                    self.base.cycle,
                    self.name(),
                    pc,
                    writeback.rd_addr,
                    writeback.num_rd_data()
                );

                Ok(Some(writeback))
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

    /// A backend EX-only library interface that accepts a single-warp issued instruction and
    /// returns a writeback bundle.
    pub fn execute(
        &mut self,
        issued: IssuedInst,
        tmask: u32,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
        smem: &mut FlatMemory,
    ) -> Writeback {
        let core_id = self.conf().lane_config.core_id;
        let rf = self.base.state.reg_file.as_mut_slice();
        let csrf = self.base.state.csr_file.as_mut_slice();

        let writeback = ExecuteUnit::execute(
            issued, core_id, self.wid, tmask, rf, csrf, scheduler, neutrino, &self.gmem, smem,
        );
        writeback
    }

    /// Fast-path that fuses frontend/backend for every warp instead of two-stage schedule/ibuf
    /// iteration.
    pub fn process(
        &mut self,
        schedule: Schedule,
        scheduler: &mut Scheduler,
        neutrino: &mut Neutrino,
        shared_mem: &mut FlatMemory,
        timing_model: &mut CoreTimingModel,
    ) -> Result<(), ExecErr> {
        let now = module_now(scheduler);
        timing_model.tick(now, scheduler);
        let ibuf = self.frontend(schedule);
        self.backend(ibuf, scheduler, neutrino, shared_mem, timing_model, now)
            .map(|_| ())
    }

    pub fn writeback(wb: &Writeback, rf: &mut [RegFile]) {
        let rd_addr = wb.rd_addr;
        for (lrf, ldata) in zip(rf, &wb.rd_data) {
            ldata.map(|data| lrf.write_gpr(rd_addr, data));
        }
    }

    pub fn set_block_threads(
        &mut self,
        block_idx: (u32, u32, u32),
        thread_idxs: &Vec<(u32, u32, u32)>,
    ) {
        assert!(thread_idxs.len() <= self.base.state.csr_file.len());
        assert!(thread_idxs.len() > 0);
        for (csr_file, thread_idx) in zip(self.base.state.csr_file.iter_mut(), thread_idxs.iter()) {
            csr_file.set_block_thread(block_idx, *thread_idx);
        }
    }

    fn issue_gmem_request(
        &self,
        opcode: u8,
        opext: u8,
        f3: u8,
        rs1_addr: u8,
        imm32: u32,
        tmask: u32,
        scheduler: &mut Scheduler,
        timing_model: &mut CoreTimingModel,
        now: Cycle,
    ) -> Result<(), ()> {
        if opext == 1 {
            return Ok(());
        }
        if opcode != Opcode::LOAD && opcode != Opcode::STORE {
            return Ok(());
        }

        let active_lanes = tmask.count_ones();
        if active_lanes == 0 {
            return Ok(());
        }

        let lane_addrs = self.collect_lane_addrs(rs1_addr, imm32, tmask);
        let bytes_per_lane = 1u32 << (f3 & 3);
        let total_bytes = bytes_per_lane.saturating_mul(active_lanes);
        let mut request =
            GmemRequest::new(self.wid, total_bytes.max(1), tmask, opcode == Opcode::LOAD);
        request.addr = lane_addrs
            .iter()
            .copied()
            .min()
            .unwrap_or(0);
        request.lane_addrs = Some(lane_addrs);

        timing_model
            .issue_gmem_request(now, self.wid, request, scheduler)
            .map(|_| ())
            .map_err(|_| ())
    }

    fn collect_lane_addrs(&self, rs1_addr: u8, imm32: u32, tmask: u32) -> Vec<u64> {
        let mut addrs = Vec::new();
        for (lane, lrf) in self.base.state.reg_file.iter().enumerate() {
            if !tmask.bit(lane) {
                continue;
            }
            let addr = lrf.read_gpr(rs1_addr).wrapping_add(imm32) as u64;
            addrs.push(addr);
        }
        addrs
    }

    fn issue_smem_request(
        &self,
        opcode: u8,
        f3: u8,
        is_load: bool,
        rs1_addr: u8,
        imm32: u32,
        tmask: u32,
        scheduler: &mut Scheduler,
        timing_model: &mut CoreTimingModel,
        now: Cycle,
    ) -> Result<(), ()> {
        if opcode != Opcode::LOAD && opcode != Opcode::STORE {
            return Ok(());
        }

        let active_lanes = tmask.count_ones();
        if active_lanes == 0 {
            return Ok(());
        }

        let lane_addrs = self.collect_lane_addrs(rs1_addr, imm32, tmask);
        let bytes_per_lane = 1u32 << (f3 & 3);
        let total_bytes = bytes_per_lane.saturating_mul(active_lanes);
        let bank = self.wid;
        let mut request = SmemRequest::new(self.wid, total_bytes.max(1), tmask, !is_load, bank);
        request.addr = lane_addrs.iter().copied().min().unwrap_or(0);
        request.lane_addrs = Some(lane_addrs);

        timing_model
            .issue_smem_request(now, self.wid, request, scheduler)
            .map(|_| ())
            .map_err(|_| ())
    }
}
