use crate::base::behavior::*;
use crate::cluster::Cluster;
use crate::command_proc::CommandProcessor;
use crate::muon::config::MuonConfig;
use crate::sim::elf::ElfBackedMem;
use crate::sim::toy_mem::ToyMemory;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};

pub static GMEM: LazyLock<RwLock<ToyMemory>> = LazyLock::new(|| RwLock::new(ToyMemory::default()));

pub struct CyclotronTopConfig {
    pub timeout: u64,
    pub elf_path: PathBuf,
    pub muon_config: MuonConfig,
}

pub struct CyclotronTop {
    pub cproc: CommandProcessor,
    pub cluster: Cluster,
    pub timeout: u64,
}

impl CyclotronTop {
    pub fn new(config: Arc<CyclotronTopConfig>) -> CyclotronTop {
        let imem = Arc::new(RwLock::new(ElfBackedMem::new(config.elf_path.as_path())));
        let top = CyclotronTop {
            cproc: CommandProcessor::new(1 /*FIXME: properly get thread dimension*/),
            cluster: Cluster::new(Arc::new(config.muon_config), imem.clone(), 0),
            timeout: config.timeout,
        };
        GMEM.write()
            .expect("gmem poisoned")
            .set_fallthrough(imem.clone());
        top
    }

    pub fn finished(&self) -> bool {
        self.cluster.all_cores_retired()
    }
}

impl ModuleBehaviors for CyclotronTop {
    fn tick_one(&mut self) {
        self.cproc.tick_one();
        let scheduled_tb = self.cproc.schedule();
        if scheduled_tb {
            self.cluster.schedule_threadblock();
        }
        self.cluster.tick_one();
        let retired_tb = self.cluster.retired_threadblock();
        self.cproc.retire(retired_tb);
    }

    fn reset(&mut self) {
        GMEM.write().expect("lock poisoned").reset();
        self.cluster.reset();
    }
}
