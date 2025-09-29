use crate::base::behavior::*;
use crate::muon::core::MuonCore;
use crate::sim::toy_mem::ToyMemory;
use log::info;
use std::sync::{Arc, RwLock};
use crate::neutrino::neutrino::Neutrino;
use crate::sim::top::ClusterConfig;
use crate::sim::log::Logger;

pub struct Cluster {
    id: usize,
    pub cores: Vec<MuonCore>,
    pub neutrino: Neutrino,
    scheduled_threadblocks: usize,
}

impl Cluster {
    pub fn new(config: Arc<ClusterConfig>, id: usize, logger: &Arc<Logger>, gmem: Arc<RwLock<ToyMemory>>) -> Self {
        let mut cores = Vec::new();
        for cid in 0..config.muon_config.num_cores {
            cores.push(MuonCore::new(Arc::new(config.muon_config), cid, logger, gmem.clone()));
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
        if self.all_cores_retired() {
            1
        } else {
            0
        }
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_cores_retired(&self) -> bool {
        self.cores.iter().all(|core| core.all_warps_retired())
    }
}

impl ModuleBehaviors for Cluster {
    fn tick_one(&mut self) {
        for core in &mut self.cores {
            core.tick_one();
            core.execute(&mut self.neutrino);
        }
        self.neutrino.tick_one();
        self.neutrino.update(&mut self.cores.iter_mut()
            .map(|c| &mut c.scheduler)
            .collect());
    }

    fn reset(&mut self) {
        for core in &mut self.cores {
            core.reset();
        }
    }
}
