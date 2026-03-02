use crate::base::{behavior::*, module::*};
use crate::info;
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::decode::{InstBuf, IssuedInst};
use crate::muon::gmem::{CorePerfSummary, CoreTimingModel};
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::muon::warp::{ExecErr, Warp, Writeback};
use crate::neutrino::neutrino::Neutrino;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use crate::sim::perf_log::PerfLogSession;
use crate::sim::trace::Tracer;
use crate::timeflow::{ClusterGmemGraph, CoreGraphConfig};
use crate::timeq::module_now;
use std::iter::zip;
use std::sync::{Arc, RwLock};

#[derive(Debug, Default)]
pub struct MuonState {}

pub struct MuonCore {
    base: ModuleBase<MuonState, MuonConfig>,
    pub scheduler: Scheduler,
    pub warps: Vec<Warp>,
    pub(crate) shared_mem: FlatMemory,

    logger: Arc<Logger>,
    tracer: Arc<Tracer>,
    timing_mode: TimingMode,
}

enum TimingMode {
    Disabled,
    Enabled(CoreTimingModel),
}

impl MuonCore {
    fn build_core(
        config: Arc<MuonConfig>,
        cluster_id: usize,
        core_id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
        timing_mode: TimingMode,
    ) -> Self {
        let num_warps = config.num_warps;
        let mut core = MuonCore {
            base: Default::default(),
            scheduler: Scheduler::new(Arc::clone(&config), core_id),
            warps: (0..num_warps)
                .map(|warp_id| {
                    Warp::new(
                        Arc::new(MuonConfig {
                            lane_config: LaneConfig {
                                warp_id,
                                core_id,
                                cluster_id,
                                ..config.lane_config
                            },
                            ..*config
                        }),
                        logger,
                        gmem.clone(),
                    )
                })
                .collect(),
            shared_mem: FlatMemory::new_with_size(config.smem_size, None),
            logger: logger.clone(),
            tracer: Arc::new(Tracer::new(&config)),
            timing_mode,
        };

        info!(
            core.logger,
            "muon core {} in cluster {} instantiated!", core_id, cluster_id
        );

        core.init_conf(Arc::clone(&config));
        core
    }

    pub fn new(
        config: Arc<MuonConfig>,
        cluster_id: usize,
        core_id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
    ) -> Self {
        Self::build_core(
            config,
            cluster_id,
            core_id,
            logger,
            gmem,
            TimingMode::Disabled,
        )
    }

    pub fn new_timed(
        config: Arc<MuonConfig>,
        cluster_id: usize,
        core_id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
        timing_config: CoreGraphConfig,
        timing_core_id: usize,
        timing_cluster_id: usize,
        cluster_gmem: Arc<RwLock<ClusterGmemGraph>>,
        perf_log_session: Option<Arc<PerfLogSession>>,
    ) -> Self {
        let num_warps = config.num_warps;
        let timing_mode = TimingMode::Enabled(CoreTimingModel::new_with_perf_log(
            timing_config,
            num_warps,
            timing_core_id,
            timing_cluster_id,
            cluster_gmem,
            perf_log_session,
            logger.clone(),
        ));
        Self::build_core(config, cluster_id, core_id, logger, gmem, timing_mode)
    }

    /// Spawn a single warp to this core.
    pub fn spawn_single_warp(&mut self) {
        self.scheduler.spawn_single_warp()
    }

    pub fn spawn_n_warps(
        &mut self,
        pc: u32,
        block_idx: (u32, u32, u32),
        thread_idxs: Vec<Vec<(u32, u32, u32)>>,
        bp: u32,
    ) {
        assert!(thread_idxs.len() <= self.conf().num_warps && !thread_idxs.is_empty());
        self.scheduler.spawn_n_warps(pc, &thread_idxs);
        for (warp, warp_thread_idxs) in self.warps.iter_mut().zip(thread_idxs.iter()) {
            warp.set_block_threads_bp(block_idx, warp_thread_idxs, bp);
        }
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_warps_retired(&self) -> bool {
        self.scheduler.all_warps_retired()
    }

    pub fn has_timing_inflight(&self) -> bool {
        match &self.timing_mode {
            TimingMode::Disabled => false,
            TimingMode::Enabled(timing_model) => {
                timing_model.outstanding_gmem() > 0 || timing_model.outstanding_smem() > 0
            }
        }
    }

    /// Schedule all warps and get per-warp PC/tmasks.
    pub fn schedule(&mut self) -> Vec<Option<Schedule>> {
        (0..self.conf().num_warps)
            .map(|wid| self.scheduler.schedule(wid))
            .collect::<Vec<_>>()
    }

    pub fn frontend(&mut self, schedules: &[Option<Schedule>]) -> InstBuf {
        let now = module_now(&self.scheduler);
        let mut ibuf_entries = Vec::with_capacity(self.warps.len());

        match &mut self.timing_mode {
            TimingMode::Enabled(timing_model) => {
                for (wid, sched_opt) in schedules.iter().enumerate() {
                    let entry = match sched_opt {
                        Some(sched) => {
                            if timing_model.allow_fetch(now, wid, sched.pc, &mut self.scheduler) {
                                Some(self.warps[wid].frontend(*sched))
                            } else {
                                None
                            }
                        }
                        None => None,
                    };
                    ibuf_entries.push(entry);
                }
            }
            TimingMode::Disabled => {
                for (wid, sched_opt) in schedules.iter().enumerate() {
                    let entry = sched_opt.map(|sched| self.warps[wid].frontend(sched));
                    ibuf_entries.push(entry);
                }
            }
        }

        InstBuf(ibuf_entries)
    }

    /// Execute decoded instructions from all active warps, i.o.w. heads of the instruction buffer,
    /// in the core backend.
    pub fn backend(&mut self, ibuf: &InstBuf, neutrino: &mut Neutrino) -> Result<(), ExecErr> {
        let now = module_now(&self.scheduler);
        let mut writebacks: Vec<Option<Writeback>> = Vec::with_capacity(self.warps.len());

        match &mut self.timing_mode {
            TimingMode::Enabled(timing_model) => {
                timing_model.tick(now, &mut self.scheduler);

                let eligible = ibuf
                    .0
                    .iter()
                    .map(|entry| entry.is_some())
                    .collect::<Vec<_>>();
                let issue_mask = timing_model.select_issue_mask(now, &eligible);
                let active_warps = self.scheduler.active_warp_mask().count_ones();
                let eligible_warps = eligible.iter().filter(|v| **v).count() as u32;
                let issued_warps = issue_mask.iter().filter(|v| **v).count() as u32;
                timing_model.record_issue_stats(now, active_warps, eligible_warps, issued_warps);

                let warps = self.warps.iter_mut();
                let ibuf_entries = ibuf.0.iter().copied();

                for (wid, (warp, ibuf_entry)) in zip(warps, ibuf_entries).enumerate() {
                    let wb_opt = match ibuf_entry {
                        Some(ib) => {
                            if !issue_mask.get(wid).copied().unwrap_or(false) {
                                self.scheduler
                                    .set_resource_wait_until(wid, Some(now.saturating_add(1)));
                                self.scheduler.replay_instruction(wid);
                                None
                            } else {
                                warp.backend_timed(
                                    ib,
                                    &mut self.scheduler,
                                    neutrino,
                                    &mut self.shared_mem,
                                    timing_model,
                                    now,
                                )?
                            }
                        }
                        None => None,
                    };
                    writebacks.push(wb_opt);
                }
            }
            TimingMode::Disabled => {
                let warps = self.warps.iter_mut();
                let ibuf_entries = ibuf.0.iter().copied();

                for (warp, ibuf_entry) in zip(warps, ibuf_entries) {
                    let wb_opt = match ibuf_entry {
                        Some(ib) => Some(warp.backend(
                            ib,
                            &mut self.scheduler,
                            neutrino,
                            &mut self.shared_mem,
                        )?),
                        None => None,
                    };
                    writebacks.push(wb_opt);
                }
            }
        }

        let tracer = Arc::get_mut(&mut self.tracer).expect("failed to get tracer");
        tracer.record(&writebacks);

        self.scheduler.tick_one();
        self.warps.iter_mut().for_each(Warp::tick_one);

        Ok(())
    }

    /// Execute an issued instruction from a single warp in the core's functional unit backend.
    pub fn execute(
        &mut self,
        warp_id: usize,
        issued: IssuedInst,
        tmask: u32,
        neutrino: &mut Neutrino,
    ) -> Writeback {
        let writeback = self.warps[warp_id].execute(
            issued,
            tmask,
            &mut self.scheduler,
            neutrino,
            &mut self.shared_mem,
        );
        self.warps[warp_id].tick_one();

        // no tracing done; instruction tracing is only enabled in the ISA-model mode
        writeback
    }

    /// Process all warps & advance each by a single instruction.
    pub fn process(&mut self, neutrino: &mut Neutrino) -> Result<(), ExecErr> {
        let schedules = self.schedule();
        let ibuf = self.frontend(&schedules);
        self.backend(&ibuf, neutrino)
    }

    /// Exposes a non-mutating fetch interface for the core.
    pub fn fetch(&self, warp: u32, pc: u32) -> u64 {
        // warp is not really necessary here
        self.warps[warp as usize].fetch(pc)
    }

    pub fn get_tracer_mut(&mut self) -> &mut Tracer {
        Arc::get_mut(&mut self.tracer).expect("failed to get tracer")
    }

    pub fn get_tracer(&self) -> &Tracer {
        &self.tracer
    }

    pub fn timing_summary(&self) -> CorePerfSummary {
        match &self.timing_mode {
            TimingMode::Disabled => {
                panic!("timing summary requested while timing mode is disabled")
            }
            TimingMode::Enabled(timing_model) => timing_model.perf_summary(),
        }
    }
}

module!(
    MuonCore,
    MuonState,
    MuonConfig,
    fn get_children(&mut self) -> Vec<&mut dyn ModuleBehaviors> {
        todo!()
    }
);

impl ModuleBehaviors for MuonCore {
    fn tick_one(&mut self) {
        self.base.cycle += 1;
    }

    fn reset(&mut self) {
        self.scheduler.reset();
        self.warps.iter_mut().for_each(Warp::reset);
    }
}

impl MuonCore {
    pub fn time(&self) -> u64 {
        self.base.cycle
    }
}
