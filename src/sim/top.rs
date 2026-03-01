use crate::base::behavior::*;
use crate::base::mem::HasMemory;
use crate::cluster::Cluster;
use crate::command_proc::CommandProcessor;
use crate::muon::config::MuonConfig;
use crate::neutrino::config::NeutrinoConfig;
use crate::sim::config::{FrontendMode, MemConfig, SimConfig};
use crate::sim::elf::ElfBackedMem;
use crate::sim::flat_mem::FlatMemory;
use crate::sim::log::Logger;
use crate::sim::perf_log::PerfLogSession;
use crate::timeflow::CoreGraphConfig;
use crate::traffic::config::TrafficConfig;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub struct Sim {
    pub config: SimConfig,
    pub top: CyclotronTop,
    pub logger: Arc<Logger>,
    perf_log_session: Option<Arc<PerfLogSession>>,
}

impl Sim {
    fn write_timing_summary(&self) {
        if self.config.timing {
            let summaries = self
                .top
                .clusters
                .iter()
                .flat_map(|cluster| cluster.cores.iter().map(|core| core.timing_summary()))
                .collect::<Vec<_>>();
            if let Some(session) = &self.perf_log_session {
                session.write_summary(summaries);
            }
        }
    }

    pub fn new(
        sim_config: SimConfig,
        muon_config: MuonConfig,
        neutrino_config: NeutrinoConfig,
        mem_config: MemConfig,
    ) -> Sim {
        Self::new_with_timing(
            sim_config,
            muon_config,
            neutrino_config,
            mem_config,
            CoreGraphConfig::default(),
            TrafficConfig::default(),
        )
    }

    pub fn new_with_timing(
        sim_config: SimConfig,
        muon_config: MuonConfig,
        neutrino_config: NeutrinoConfig,
        mem_config: MemConfig,
        timing_config: CoreGraphConfig,
        traffic_config: TrafficConfig,
    ) -> Sim {
        let perf_log_session = if sim_config.timing {
            PerfLogSession::new().map(Arc::new)
        } else {
            None
        };
        let logger = Arc::new(Logger::new(sim_config.log_level));
        let top = CyclotronTop::new(
            Arc::new(CyclotronConfig {
                timeout: sim_config.timeout,
                elf: sim_config.elf.clone(),
                cluster_config: ClusterConfig {
                    muon_config,
                    neutrino_config,
                    timing_config,
                },
                mem_config,
                timing_enabled: sim_config.timing,
                frontend_mode: sim_config.frontend_mode,
                traffic_config,
            }),
            &logger,
            perf_log_session.clone(),
        );

        let mut sim = Sim {
            config: sim_config,
            top,
            logger,
            perf_log_session,
        };
        sim.top.reset();
        sim
    }

    pub fn simulate(&mut self) -> Result<(), u32> {
        self.top.reset();
        for cycle in 0..self.top.timeout {
            if self.top.finished() {
                println!("simulation finished after {} cycles", cycle + 1);
                self.write_timing_summary();
                return self.check_tohost();
            }
            self.top.tick_one();
        }

        self.write_timing_summary();

        Err(0)
    }

    pub fn check_tohost(&self) -> Result<(), u32> {
        if let Some(tohost) = self.top.clusters[0].cores[0].scheduler.tohost() {
            if tohost != 0 {
                let case = tohost >> 1;
                println!(
                    "Cyclotron: isa-test failed with tohost={}, case={}",
                    tohost, case
                );
                return Err(tohost);
            } else {
                println!("Cyclotron: isa-test passed with tohost={}", tohost);
            }
        }
        return Ok(());
    }

    /// Advances all cores by one instruction.
    pub fn tick(&mut self) {
        if self.top.finished() {
            return;
        }
        self.top.tick_one();
    }

    pub fn finished(&self) -> bool {
        self.top.finished()
    }
}

pub struct CyclotronConfig {
    pub timeout: u64, // TODO: use sim
    pub elf: PathBuf, // TODO: use sim
    pub cluster_config: ClusterConfig,
    pub mem_config: MemConfig,
    pub timing_enabled: bool,
    pub frontend_mode: FrontendMode,
    pub traffic_config: TrafficConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClusterConfig {
    pub muon_config: MuonConfig,
    pub neutrino_config: NeutrinoConfig,
    pub timing_config: CoreGraphConfig,
}

pub struct CyclotronTop {
    pub cproc: CommandProcessor,
    pub clusters: Vec<Cluster>,
    pub timeout: u64,
    pub gmem: Arc<RwLock<FlatMemory>>,
}

impl CyclotronTop {
    pub fn new(
        config: Arc<CyclotronConfig>,
        logger: &Arc<Logger>,
        perf_log_session: Option<Arc<PerfLogSession>>,
    ) -> CyclotronTop {
        let elf_path = Path::new(&config.elf);
        let imem = ElfBackedMem::new(&elf_path);
        let mut clusters = Vec::new();
        let cluster_config = Arc::new(config.cluster_config.clone());

        // TODO: current implementation means imem is writable, but this is true
        // in hardware too?
        let mut gmem = FlatMemory::new(Some(config.mem_config));
        gmem.copy_elf(&imem);

        let gmem = Arc::new(RwLock::new(gmem));

        let num_clusters = 1;
        let cores_per_cluster = cluster_config.muon_config.num_cores.max(1);
        if config.timing_enabled {
            let gmem_timing = Arc::new(RwLock::new(crate::timeflow::ClusterGmemGraph::new(
                cluster_config.timing_config.memory.gmem.clone(),
                num_clusters,
                cores_per_cluster,
            )));
            for id in 0..num_clusters {
                clusters.push(Cluster::new_timed(
                    cluster_config.clone(),
                    id,
                    logger,
                    gmem.clone(),
                    gmem_timing.clone(),
                    perf_log_session.clone(),
                    config.frontend_mode,
                    config.traffic_config.clone(),
                ));
            }
        } else {
            for id in 0..num_clusters {
                clusters.push(Cluster::new(
                    cluster_config.clone(),
                    id,
                    logger,
                    gmem.clone(),
                    config.frontend_mode,
                    config.traffic_config.clone(),
                ));
            }
        }
        CyclotronTop {
            cproc: CommandProcessor::new(
                Arc::new(config.cluster_config.muon_config.clone()),
                1, /*FIXME: properly get thread dimension*/
            ),
            clusters,
            timeout: config.timeout,
            gmem,
        }
    }

    pub fn schedule_clusters(&mut self) {
        if self
            .clusters
            .first()
            .map(|cluster| !cluster.uses_command_processor())
            .unwrap_or(false)
        {
            return;
        }
        let tb_schedule = self.cproc.schedule();
        let num_clusters = self.clusters.len();
        for id in 0..num_clusters {
            if tb_schedule[id] {
                self.clusters[id].schedule_threadblock();
            }
        }
    }

    /// Load a word from gmem.
    pub fn gmem_load(&self, addr: u32) -> [u8; 4] {
        self.gmem
            .read()
            .expect("lock poisoned")
            .read_n::<4>(addr as usize)
            .expect("load failed")
    }

    /// Store a data with given size to gmem.  Doesn't support partial writes.
    pub fn gmem_store(&self, addr: u32, data: u32, size: u32) {
        let mut gmem = self.gmem.write().expect("lock poisoned");
        let data_bytes = data.to_le_bytes();
        match size {
            0 => gmem.write(addr as usize, &data_bytes[0..1]), // store byte
            1 => gmem.write(addr as usize, &data_bytes[0..2]), // store half
            2 => gmem.write(addr as usize, &data_bytes[0..4]), // store word
            _ => panic!("unimplemented store type"),
        }
        .expect("store failed");
    }

    pub fn finished(&self) -> bool {
        self.clusters.iter().all(|cl| cl.all_cores_retired())
    }
}

impl ModuleBehaviors for CyclotronTop {
    fn tick_one(&mut self) {
        self.schedule_clusters();

        self.clusters.iter_mut().for_each(Cluster::tick_one);

        if self
            .clusters
            .first()
            .map(|cluster| !cluster.uses_command_processor())
            .unwrap_or(false)
        {
            return;
        }

        // FIXME: if tick_one() is called after finished() is true, the retire() call might result
        // in an underflow.
        let num_clusters = self.clusters.len();
        for id in 0..num_clusters {
            let retired_tb = self.clusters[id].retired_threadblock();
            self.cproc.retire(id, retired_tb);
        }
    }

    fn reset(&mut self) {
        self.clusters.iter_mut().for_each(Cluster::reset);
    }
}
