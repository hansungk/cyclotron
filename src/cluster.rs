use crate::base::behavior::*;
use crate::base::mem::*;
use crate::muon::config::MuonConfig;
use crate::muon::core::MuonCore;
use crate::sim::elf::ElfBackedMem;
use log::info;
use std::sync::{Arc, RwLock};

pub struct Cluster {
    id: usize,
    imem: Arc<RwLock<ElfBackedMem>>,
    cores: Vec<MuonCore>,
    scheduled_threadblocks: usize,
}

impl Cluster {
    pub fn new(config: Arc<MuonConfig>, imem: Arc<RwLock<ElfBackedMem>>, id: usize) -> Self {
        let mut cores = Vec::new();
        for cid in 0..1 {
            cores.push(MuonCore::new(Arc::clone(&config), cid));
        }
        Cluster {
            id,
            imem,
            cores,
            scheduled_threadblocks: 0,
        }
    }

    pub fn schedule_threadblock(&mut self) {
        assert!(
            self.scheduled_threadblocks == 0,
            "attempted to schedule a threadblock to an already-busy cluster. TODO: support more than one outstanding threadblocks"
        );
        info!("cluster {}: scheduled a threadblock", self.id);
        for core in &mut self.cores {
            core.spawn_warp();
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
