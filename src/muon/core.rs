use std::sync::Arc;
use log::debug;
use crate::sim::log::Logger;
use crate::info;
use crate::base::{behavior::*, module::*};
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::muon::warp::Warp;
use crate::muon::decode::{InstBuf, InstBufEntry};
use crate::neutrino::neutrino::Neutrino;

#[derive(Debug, Default)]
pub struct MuonState {}

pub struct MuonCore {
    pub base: ModuleBase<MuonState, MuonConfig>,
    pub id: usize,
    pub scheduler: Scheduler,
    pub warps: Vec<Warp>,
    logger: Arc<Logger>,
}

impl MuonCore {
    pub fn new(config: Arc<MuonConfig>, id: usize, logger: &Arc<Logger>) -> Self {
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
            }), logger)).collect(),
            logger: logger.clone(),
        };

        info!(core.logger, "muon core {} instantiated!", config.lane_config.core_id);

        core.init_conf(Arc::clone(&config));
        core
    }

    /// Spawn a warp in this core.  Invoked by the command processor to schedule a threadblock to
    /// the cluster.
    pub fn spawn_warp(&mut self) {
        self.scheduler.spawn_warp()
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_warps_retired(&self) -> bool {
        self.scheduler.all_warps_retired()
    }

    pub fn schedule(&mut self) -> Vec<Option<Schedule>> {
        let schedules = (0..self.conf().num_warps)
            .map(|wid| self.scheduler.get_schedule(wid))
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

    pub fn backend(&mut self, ibuf: &InstBuf, neutrino: &mut Neutrino) {
        self.warps.iter_mut().zip(&ibuf.0).for_each(|(warp, ibuf)| {
            ibuf.as_ref().map(|ib| {
                warp.backend(ib, &mut self.scheduler, neutrino);
            });
        });
        self.scheduler.tick_one();
        self.warps.iter_mut().for_each(Warp::tick_one);
    }

    pub fn execute(&mut self, neutrino: &mut Neutrino) {
        let schedules = self.schedule();
        let ibuf = self.frontend(&schedules);
        self.backend(&ibuf, neutrino);
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
