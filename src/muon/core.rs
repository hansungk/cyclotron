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
    pub imem_req: Vec<Port<OutputPort, MemRequest>>,
    pub imem_resp: Vec<Port<InputPort, MemResponse>>,
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
            imem_req: fill!(Port::new(), num_warps),
            imem_resp: fill!(Port::new(), num_warps),
        };

        let sched_out = me.scheduler.schedule.iter_mut();
        let sched_in = me.warps.iter_mut().map(|w| &mut w.schedule);
        link_iter(sched_in, sched_out);

        let sched_wb_in = me.scheduler.schedule_wb.iter_mut();
        let sched_wb_out = me.warps.iter_mut().map(|w| &mut w.schedule_wb);
        link_iter(sched_wb_in, sched_wb_out);

        let imem_req_warps = me.warps.iter_mut().map(|w| &mut w.imem_req);
        let imem_req_core = me.imem_req.iter_mut();
        imem_req_warps.for_each(|w| { tie_off(w); });
        imem_req_core.for_each(|w| { tie_off(w); });

        let imem_resp_warps = &mut me.warps.iter_mut().map(|w| &mut w.imem_resp);
        let imem_resp_core = me.imem_resp.iter_mut();
        imem_resp_core.for_each(|w| { tie_off_input(w); });
        imem_resp_warps.for_each(|w| { tie_off_input(w); });

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
        if let Some(sched) = self.scheduler.schedule[0].peek() {
            info!("warp 0 schedule=0x{:08x}", sched.pc);
            assert_eq!(self.warps[0].schedule.peek().unwrap().pc, sched.pc);
        }

        self.warps.iter_mut().for_each(Warp::tick_one);

        // forward output ports
        let imem_req_warps = self.warps.iter_mut().map(|w| &mut w.imem_req);
        let imem_req_core = self.imem_req.iter_mut();
        for (req_warp, req_core) in imem_req_warps.zip(imem_req_core) {
            if let Some(req) = req_warp.get() {
                req_core.put(&req);
            }
        }

        // forward input ports
        let imem_resp_warps = self.warps.iter_mut().map(|w| &mut w.imem_resp);
        let imem_resp_core = self.imem_resp.iter_mut();
        for (resp_warp, resp_core) in imem_resp_warps.zip(imem_resp_core) {
            if let Some(resp) = resp_core.get() {
                resp_warp.put(&resp);
            }
        }

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
