use std::sync::{Arc, RwLock};
use crate::sim::log::Logger;
use crate::sim::trace::Tracer;
use crate::info;
use crate::base::{behavior::*, module::*};
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::muon::warp::{ExecErr, Warp, Writeback};
use crate::muon::decode::InstBuf;
use crate::neutrino::neutrino::Neutrino;
use crate::sim::toy_mem::ToyMemory;

#[derive(Debug, Default)]
pub struct MuonState {}

pub struct MuonCore {
    pub base: ModuleBase<MuonState, MuonConfig>,
    pub id: usize,
    pub scheduler: Scheduler,
    warps: Vec<Warp>,
    logger: Arc<Logger>,
    tracer: Arc<Tracer>,
}

impl MuonCore {
    pub fn new(config: Arc<MuonConfig>, id: usize, logger: &Arc<Logger>, gmem: Arc<RwLock<ToyMemory>>) -> Self {
        let num_warps = config.num_warps;
        let mut core = MuonCore {
            base: Default::default(),
            id,
            scheduler: Scheduler::new(Arc::clone(&config), id),
            warps: (0..num_warps).map(|warp_id| Warp::new(Arc::new(MuonConfig {
                lane_config: LaneConfig {
                    warp_id,
                    core_id: id,
                    ..config.lane_config
                },
                ..*config
            }), logger, gmem.clone())).collect(),
            logger: logger.clone(),
            tracer: Arc::new(Tracer::new(&config)),
        };

        info!(core.logger, "muon core {} instantiated!", id);

        core.init_conf(Arc::clone(&config));
        core
    }

    /// Spawn a single warp to this core.
    pub fn spawn_single_warp(&mut self) {
        self.scheduler.spawn_single_warp()
    }

    pub fn spawn_n_warps(&mut self, pc: u32, n: usize) {
        self.scheduler.spawn_n_warps(pc, n)
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_warps_retired(&self) -> bool {
        self.scheduler.all_warps_retired()
    }

    pub fn schedule(&mut self) -> Vec<Option<Schedule>> {
        let schedules = (0..self.conf().num_warps)
            .map(|wid| { self.scheduler.get_schedule(wid) })
            .collect::<Vec<_>>();
        schedules
    }

    pub fn frontend(&mut self, schedules: &Vec<Option<Schedule>>) -> InstBuf {
        let ibuf_entries = self.warps.iter_mut().zip(schedules).map(|(warp, sched)| {
            sched.as_ref().map(|s| {
                warp.frontend(s.clone())
            })
        });

        InstBuf(ibuf_entries.collect())
    }

    pub fn backend(&mut self, ibuf: &InstBuf, neutrino: &mut Neutrino) -> Result<(), ExecErr> {
        let mut writebacks: Vec<Option<Writeback>> = Vec::with_capacity(self.warps.len());
        for (warp, ibuf) in self.warps.iter_mut().zip(&ibuf.0) {
            let wb = match ibuf.as_ref() {
                Some(ib) => Some(warp.backend(ib, &mut self.scheduler, neutrino)?),
                None => None,
            };
            writebacks.push(wb);
        }

        let tracer = Arc::get_mut(&mut self.tracer).expect("failed to get tracer");
        tracer.record(&writebacks);

        self.scheduler.tick_one();
        self.warps.iter_mut().for_each(Warp::tick_one);

        Ok(())
    }

    pub fn execute(&mut self, neutrino: &mut Neutrino) -> Result<(), ExecErr> {
        let schedules = self.schedule();
        let ibuf = self.frontend(&schedules);
        self.backend(&ibuf, neutrino)
    }

    pub fn get_tracer(&mut self) -> &mut Tracer {
        Arc::get_mut(&mut self.tracer).expect("failed to get tracer")
    }
}

module!(MuonCore, MuonState, MuonConfig,
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
