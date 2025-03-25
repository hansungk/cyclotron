use std::sync::Arc;
use log::info;
use crate::base::{behavior::*, module::*, port::*};
use crate::base::mem::{MemRequest, MemResponse};
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::scheduler::Scheduler;
use crate::muon::warp::Warp;
use crate::utils::fill;

#[derive(Debug, Default)]
pub struct MuonState {}

#[derive(Debug, Default)]
pub struct MuonCore {
    pub base: ModuleBase<MuonState, MuonConfig>,
    pub id: usize,
    pub scheduler: Scheduler,
    pub warps: Vec<Warp>,
}

impl MuonCore {
    pub fn new(config: Arc<MuonConfig>, id: usize) -> Self {
        let num_warps = config.num_warps;
        let mut me = MuonCore {
            base: Default::default(),
            id,
            scheduler: Scheduler::new(Arc::clone(&config)),
            warps: (0..num_warps).map(|warp_id| Warp::new(Arc::new(MuonConfig {
                lane_config: LaneConfig {
                    warp_id,
                    ..config.lane_config
                },
                ..*config
            }))).collect(),
        };

        info!("muon core {} instantiated!", config.lane_config.core_id);

        me.init_conf(Arc::clone(&config));
        me
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
}

module!(MuonCore, MuonState, MuonConfig,
    fn get_children(&mut self) -> Vec<&mut dyn ModuleBehaviors> {
        todo!()
    }
);

impl ModuleBehaviors for MuonCore {
    fn tick_one(&mut self) {
        // println!("{}: muon tick!", self.base.cycle);
        self.scheduler.tick_one();

        // forward schedule to warps
        let sched_out = self.scheduler.schedule.iter_mut();
        let sched_in = self.warps.iter_mut().map(|w| &mut w.schedule);
        sched_out.zip(sched_in).for_each(|(out, in_)| { *in_ = out.clone(); });

        if let Some(sched0) = self.scheduler.schedule[0].as_ref() {
            info!("warp 0 schedule=0x{:08x}", sched0.pc);
        }

        self.warps.iter_mut().for_each(Warp::tick_one);

        // forward schedule_wb back to scheduler
        let sched_wb_in = self.scheduler.schedule_wb.iter_mut();
        let sched_wb_out = self.warps.iter_mut().map(|w| &mut w.schedule_wb);
        sched_wb_out.zip(sched_wb_in).for_each(|(out, in_)| { *in_ = out.clone(); });

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
