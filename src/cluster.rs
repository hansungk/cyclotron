use crate::base::behavior::*;
use crate::base::component::IsComponent;
use crate::base::mem::*;
use crate::muon::config::MuonConfig;
use crate::muon::core::MuonCore;
use crate::sim::elf::ElfBackedMem;
use std::sync::{Arc, RwLock};

pub struct Cluster {
    imem: Arc<RwLock<ElfBackedMem>>,
    muon: MuonCore,
}

impl Cluster {
    pub fn new(config: Arc<MuonConfig>, imem: Arc<RwLock<ElfBackedMem>>) -> Self {
        Cluster {
            imem,
            muon: MuonCore::new(config),
        }
    }

    pub fn tick_one(&mut self) {
        for (ireq, iresp) in &mut self.muon.imem_req.iter_mut().zip(&mut self.muon.imem_resp) {
            if let Some(req) = ireq.get() {
                assert_eq!(req.size, 8, "imem read request is not 8 bytes");
                let inst = self
                    .imem
                    .write()
                    .expect("lock poisoned")
                    .read_inst(req.address)
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

    pub fn reset(&mut self) {
        self.muon.reset();
    }
}
