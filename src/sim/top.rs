use crate::base::behavior::*;
use crate::base::component::IsComponent;
use crate::cluster::Cluster;
use crate::muon::config::MuonConfig;
use crate::sim::elf::{ElfBackedMem, ElfBackedMemConfig};
use crate::sim::toy_mem::ToyMemory;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};

pub static GMEM: LazyLock<RwLock<ToyMemory>> = LazyLock::new(|| RwLock::new(ToyMemory::default()));

#[derive(Default, Clone)]
pub struct CyclotronTopConfig {
    pub timeout: u64,
    pub elf_path: PathBuf,
    pub muon_config: MuonConfig,
}

pub struct CyclotronTop {
    pub cluster: Cluster,
    pub timeout: u64,
}

impl CyclotronTop {
    pub fn new(config: Arc<CyclotronTopConfig>) -> CyclotronTop {
        let imem = Arc::new(RwLock::new(ElfBackedMem::new(Arc::new(
            ElfBackedMemConfig {
                path: config.elf_path.clone(),
            },
        ))));

        let me = CyclotronTop {
            // imem: imem.clone(),
            // muon: MuonCore::new(Arc::new(config.muon_config)),
            cluster: Cluster::new(Arc::new(config.muon_config), imem.clone()),
            timeout: config.timeout,
        };
        GMEM.write()
            .expect("gmem poisoned")
            .set_fallthrough(imem.clone());

        me
    }
}

impl ComponentBehaviors for CyclotronTop {
    fn tick_one(&mut self) {
        self.cluster.tick_one();
    }

    fn reset(&mut self) {
        GMEM.write().expect("lock poisoned").reset();
        self.cluster.reset();
    }
}
