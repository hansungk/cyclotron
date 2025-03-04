use crate::base::behavior::*;
use crate::base::mem::*;
use crate::muon::config::MuonConfig;
use crate::muon::core::MuonCore;
use crate::sim::elf::ElfBackedMem;
use std::sync::{Arc, RwLock};

pub struct Cluster {
    imem: Arc<RwLock<ElfBackedMem>>,
    cores: Vec<MuonCore>,
}

impl Cluster {
    pub fn new(config: Arc<MuonConfig>, imem: Arc<RwLock<ElfBackedMem>>) -> Self {
        let mut cores = Vec::new();
        for id in 0..1 {
            cores.push(MuonCore::new(config.clone(), id));
        }
        Cluster { imem, cores }
    }
}

impl ModuleBehaviors for Cluster {
    fn tick_one(&mut self) {
        for core in &mut self.cores {
            for (ireq, iresp) in &mut core.imem_req.iter_mut().zip(&mut core.imem_resp) {
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
            core.tick_one();
        }
    }

    fn reset(&mut self) {
        for core in &mut self.cores {
            core.reset();
        }
    }
}
