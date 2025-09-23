use crate::base::behavior::*;
use crate::cluster::Cluster;
use crate::command_proc::CommandProcessor;
use crate::muon::config::MuonConfig;
use crate::base::module::IsModule;
use crate::sim::config::{SimConfig, MemConfig};
use crate::sim::elf::ElfBackedMem;
use crate::sim::toy_mem::ToyMemory;
use crate::sim::log::Logger;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};
use serde::Deserialize;
use crate::neutrino::config::NeutrinoConfig;

pub static GMEM: LazyLock<RwLock<ToyMemory>> = LazyLock::new(|| RwLock::new(ToyMemory::default()));

pub struct Sim {
    pub config: SimConfig,
    pub top: CyclotronTop,
    pub logger: Arc<Logger>,
}

impl Sim {
    pub fn new(sim_config: SimConfig, muon_config: MuonConfig,
               neutrino_config: NeutrinoConfig,
               mem_config: MemConfig) -> Sim {
        let logger = Arc::new(Logger::new());
        let top = CyclotronTop::new(Arc::new(CyclotronConfig {
            timeout: sim_config.timeout,
            elf: sim_config.elf.clone(),
            cluster_config: ClusterConfig {
                muon_config,
                neutrino_config,
            },
            mem_config,
        }), &logger);
        let sim = Sim {
            config: sim_config,
            top,
            logger,
        };
        sim
    }

    pub fn simulate(&mut self) -> Result<(), u32> {
        self.top.reset();
        for cycle in 0..self.top.timeout {
            self.top.tick_one();
            if self.top.finished() {
                println!("simulation finished after {} cycles", cycle + 1);
                if let Some(mut tohost) = self.top.clusters[0].cores[0].scheduler.state().tohost {
                    if tohost > 0 {
                        tohost >>= 1;
                        println!("failed test case {}", tohost);
                        return Err(tohost)
                    } else {
                        println!("test passed");
                    }
                }
                return Ok(());
            }
        }
        Err(0)
    }
}

pub struct CyclotronConfig {
    pub timeout: u64, // TODO: use sim
    pub elf: String, // TODO: use sim
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
    pub fn new(config: Arc<CyclotronConfig>, logger: &Arc<Logger>) -> CyclotronTop {
        let elf_path = Path::new(&config.elf);
        let imem = Arc::new(RwLock::new(ElfBackedMem::new(&elf_path)));
        let mut clusters = Vec::new();
        let cluster_config = Arc::new(config.cluster_config.clone());
        for id in 0..1 {
            clusters.push(Cluster::new(cluster_config.clone(), id, logger));
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
