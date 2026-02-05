use crate::base::behavior::*;
use crate::muon::core::MuonCore;
use crate::neutrino::neutrino::Neutrino;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use crate::sim::top::ClusterConfig;
use log::info;
use std::sync::{Arc, RwLock};

pub struct Cluster {
    id: usize,
    pub cores: Vec<MuonCore>,
    pub neutrino: Neutrino,
    scheduled_threadblocks: usize,
}

impl Cluster {
    pub fn new(
        config: Arc<ClusterConfig>,
        id: usize,
        logger: &Arc<Logger>,
        gmem: Arc<RwLock<FlatMemory>>,
        gmem_timing: Arc<RwLock<crate::timeflow::ClusterGmemGraph>>,
    ) -> Self {
        let mut cores = Vec::new();
        for cid in 0..config.muon_config.num_cores {
            let timing_core_id = id * config.muon_config.num_cores + cid;
            cores.push(MuonCore::new(
                Arc::new(config.muon_config),
                cid,
                logger,
                gmem.clone(),
                config.timing_config.clone(),
                timing_core_id,
                id,
                gmem_timing.clone(),
            ));
        }
        Cluster {
            id,
            cores,
            neutrino: Neutrino::new(Arc::new(config.neutrino_config)),
            scheduled_threadblocks: 0,
        }
    }

    pub fn schedule_threadblock(&mut self) {
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
        self.cores
            .iter()
            .all(|core| core.all_warps_retired() && !core.has_timing_inflight())
    }
}

impl ModuleBehaviors for Cluster {
    fn tick_one(&mut self) {
        for core in &mut self.cores {
            core.tick_one();
            core.process(&mut self.neutrino).unwrap();
        }
        self.neutrino.tick_one();
        self.neutrino
            .update(&mut self.cores.iter_mut().map(|c| &mut c.scheduler).collect());
    }

    fn reset(&mut self) {
        for core in &mut self.cores {
            core.reset();
        }
    }
}
