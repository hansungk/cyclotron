use crate::base::behavior::*;
use crate::muon::core::MuonCore;
use crate::neutrino::neutrino::Neutrino;
use crate::sim::config::FrontendMode;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use crate::sim::perf_log::PerfLogSession;
use crate::sim::top::ClusterConfig;
use crate::traffic::config::TrafficConfig;
use crate::traffic::smem_driver::SmemTrafficDriver;
use log::{info, warn};
use std::sync::{Arc, RwLock};

pub struct Cluster {
    id: usize,
    pub cores: Vec<MuonCore>,
    pub neutrino: Neutrino,
    scheduled_threadblocks: usize,
    frontend: ClusterFrontend,
}

enum ClusterFrontend {
    Elf,
    TrafficSmem(SmemTrafficDriver),
}

impl Cluster {
    pub fn new(
        config: Arc<ClusterConfig>,
        id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
        frontend_mode: FrontendMode,
        traffic_config: TrafficConfig,
    ) -> Self {
        let mut cores = Vec::new();
        for cid in 0..config.muon_config.num_cores {
            cores.push(MuonCore::new(
                Arc::new(config.muon_config),
                id,
                cid,
                logger,
                gmem.clone(),
            ));
        }
        let frontend = match frontend_mode {
            FrontendMode::Elf => ClusterFrontend::Elf,
            FrontendMode::TrafficSmem => {
                if !traffic_config.enabled {
                    warn!(
                        "frontend_mode=traffic_smem set, but [traffic].enabled=false: running STF skeleton anyway"
                    );
                }
                ClusterFrontend::TrafficSmem(SmemTrafficDriver::new(&traffic_config))
            }
        };
        Cluster {
            id,
            cores,
            neutrino: Neutrino::new(Arc::new(config.neutrino_config)),
            scheduled_threadblocks: 0,
            frontend,
        }
    }

    pub fn new_timed(
        config: Arc<ClusterConfig>,
        id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
        gmem_timing: Arc<RwLock<crate::timeflow::ClusterGmemGraph>>,
        perf_log_session: Option<Arc<PerfLogSession>>,
        frontend_mode: FrontendMode,
        traffic_config: TrafficConfig,
    ) -> Self {
        let mut cores = Vec::new();
        for cid in 0..config.muon_config.num_cores {
            let timing_core_id = id * config.muon_config.num_cores + cid;
            cores.push(MuonCore::new_timed(
                Arc::new(config.muon_config),
                id,
                cid,
                logger,
                gmem.clone(),
                config.timing_config.clone(),
                timing_core_id,
                id,
                gmem_timing.clone(),
                perf_log_session.clone(),
            ));
        }
        let frontend = match frontend_mode {
            FrontendMode::Elf => ClusterFrontend::Elf,
            FrontendMode::TrafficSmem => {
                if !traffic_config.enabled {
                    warn!(
                        "frontend_mode=traffic_smem set, but [traffic].enabled=false: running STF skeleton anyway"
                    );
                }
                ClusterFrontend::TrafficSmem(SmemTrafficDriver::new(&traffic_config))
            }
        };
        Cluster {
            id,
            cores,
            neutrino: Neutrino::new(Arc::new(config.neutrino_config)),
            scheduled_threadblocks: 0,
            frontend,
        }
    }

    pub fn schedule_threadblock(&mut self) {
        if !matches!(self.frontend, ClusterFrontend::Elf) {
            return;
        }
        assert_eq!(
            self.scheduled_threadblocks, 0,
            "attempted to schedule a threadblock to an already-busy cluster. TODO: support more than one outstanding threadblocks"
        );
        info!("cluster {}: scheduled a threadblock", self.id);
        for core in &mut self.cores {
            core.spawn_single_warp();
        }
        self.scheduled_threadblocks += 1;
    }

    pub fn retired_threadblock(&self) -> usize {
        self.all_cores_retired() as usize
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_cores_retired(&self) -> bool {
        match &self.frontend {
            ClusterFrontend::Elf => self
                .cores
                .iter()
                .all(|core| core.all_warps_retired() && !core.has_timing_inflight()),
            ClusterFrontend::TrafficSmem(driver) => {
                driver.is_done() && self.cores.iter().all(|core| !core.has_timing_inflight())
            }
        }
    }

    pub fn uses_command_processor(&self) -> bool {
        matches!(self.frontend, ClusterFrontend::Elf)
    }
}

impl ModuleBehaviors for Cluster {
    fn tick_one(&mut self) {
        match &mut self.frontend {
            ClusterFrontend::Elf => {
                for core in &mut self.cores {
                    core.tick_one();
                    core.process(&mut self.neutrino).unwrap();
                }
                self.neutrino.tick_one();
                self.neutrino
                    .update(&mut self.cores.iter_mut().map(|c| &mut c.scheduler).collect());
            }
            ClusterFrontend::TrafficSmem(driver) => {
                for core in &mut self.cores {
                    core.tick_one();
                }
                driver.tick();
            }
        }
    }

    fn reset(&mut self) {
        for core in &mut self.cores {
            core.reset();
        }
    }
}
