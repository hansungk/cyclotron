use std::iter::Iterator;
use std::sync::{Arc, LazyLock, RwLock};
use crate::base::mem::*;
use crate::base::behavior::*;
use crate::base::component::IsComponent;
use crate::muon::core::MuonCore;
use crate::muon::config::MuonConfig;
use crate::sim::elf::{ElfBackedMem, ElfBackedMemConfig};
use crate::sim::toy_mem::ToyMemory;


pub static GMEM: LazyLock<RwLock<ToyMemory>> = LazyLock::new(|| RwLock::new(ToyMemory::default()));

#[derive(Default, Clone)]
pub struct CyclotronTopConfig {
    pub timeout: u64,
    pub elf_path: String,
    pub muon_config: MuonConfig,
}

pub struct CyclotronTop {
    pub imem: Arc<RwLock<ElfBackedMem>>,
    pub muon: MuonCore,

    pub timeout: u64
}

impl CyclotronTop {
    pub fn new(config: Arc<CyclotronTopConfig>) -> CyclotronTop {
        let imem = Arc::new(RwLock::new(
            ElfBackedMem::new(Arc::new(ElfBackedMemConfig {
                path: config.elf_path.clone(),
            }))
        ));
        let me = CyclotronTop {
            imem: imem.clone(),
            muon: MuonCore::new(Arc::new(config.muon_config)),
            timeout: config.timeout,
        };
        GMEM.write().expect("gmem poisoned").set_fallthrough(imem.clone());

        me
    }
}

impl ComponentBehaviors for CyclotronTop {
    fn tick_one(&mut self) {
        for (ireq, iresp) in &mut self.muon.imem_req.iter_mut().zip(&mut self.muon.imem_resp) {
            if let Some(req) = ireq.get() {
                assert_eq!(req.size, 8, "imem read request is not 8 bytes");
                let inst = self.imem.write().expect("lock poisoned").read_inst(req.address)
                    .expect(&format!("invalid pc: 0x{:x}", req.address));
                let succ = iresp.put(&MemResponse {
                    op: MemRespOp::Ack,
                    data: Some(Arc::new(inst.to_le_bytes())),
                });
                assert!(succ, "muon asserted fetch pressure, not implemented");
            }
        }
        self.muon.tick_one();
    }

    fn reset(&mut self) {
        GMEM.write().expect("lock poisoned").reset();
        self.imem.write().expect("lock poisoned").reset();
        self.muon.reset();
    }
}
