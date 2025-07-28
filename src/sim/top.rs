use crate::base::behavior::*;
use crate::cluster::Cluster;
use crate::command_proc::CommandProcessor;
use crate::muon::config::MuonConfig;
use crate::sim::config::MemConfig;
use crate::sim::elf::ElfBackedMem;
use crate::sim::toy_mem::ToyMemory;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};
use serde::Deserialize;
use crate::neutrino::config::NeutrinoConfig;

pub static GMEM: LazyLock<RwLock<ToyMemory>> = LazyLock::new(|| RwLock::new(ToyMemory::default()));

pub struct CyclotronTopConfig {
    pub timeout: u64,
    pub elf_path: PathBuf,
    pub cluster_config: ClusterConfig,
    pub mem_config: MemConfig,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct ClusterConfig {
    pub muon_config: MuonConfig,
    pub neutrino_config: NeutrinoConfig,
}

pub struct CyclotronTop {
    pub cproc: CommandProcessor,
    pub clusters: Vec<Cluster>,
    pub timeout: u64,
}

impl CyclotronTop {
    pub fn new(config: Arc<CyclotronTopConfig>) -> CyclotronTop {
        let imem = Arc::new(RwLock::new(ElfBackedMem::new(config.elf_path.as_path())));
        let mut clusters = Vec::new();
        let cluster_config = Arc::new(config.cluster_config.clone());
        for id in 0..1 {
            clusters.push(Cluster::new(cluster_config.clone(), id));
        }
        let top = CyclotronTop {
            cproc: CommandProcessor::new(Arc::new(config.cluster_config.muon_config.clone()),
                1 /*FIXME: properly get thread dimension*/),
            clusters,
            timeout: config.timeout,
        };
        GMEM.write()
            .expect("gmem poisoned")
            .set_fallthrough(Arc::clone(&imem));

        GMEM.write()
            .expect("gmem poisoned")
            .set_config(config.mem_config);
        top
    }

    pub fn finished(&self) -> bool {
        self.clusters.iter().all(|cl| cl.all_cores_retired())
    }
}

impl ModuleBehaviors for CyclotronTop {
    fn tick_one(&mut self) {
        self.cproc.tick_one();
        let tb_schedule = self.cproc.schedule();
        let num_clusters = self.clusters.len();
        for id in 0..num_clusters {
            if tb_schedule[id] {
                self.clusters[id].schedule_threadblock();
            }
        }

        self.clusters.iter_mut().for_each(Cluster::tick_one);

        for id in 0..num_clusters {
            let retired_tb = self.clusters[id].retired_threadblock();
            self.cproc.retire(id, retired_tb);
        }
    }

    fn reset(&mut self) {
        GMEM.write().expect("lock poisoned").reset();
        self.clusters.iter_mut().for_each(Cluster::reset);
    }
}
