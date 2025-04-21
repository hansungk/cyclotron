use crate::base::behavior::*;
use crate::base::mem::{HasMemory, MemResponse};
use crate::base::module::{module, IsModule, ModuleBase};
use crate::base::port::{OutputPort, Port};
use crate::muon::config::MuonConfig;
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, DecodedInst, RegFile};
use crate::muon::execute::{ExecuteUnit, SFUType, Writeback};
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::sim::top::GMEM;
use log::info;
use std::sync::Arc;
use crate::neutrino::neutrino::Neutrino;

#[derive(Debug, Default)]
pub struct WarpState {
    pub stalled: bool,
    pub reg_file: Vec<RegFile>,
    pub csr_file: Vec<CSRFile>,
}

#[derive(Debug, Default)]
pub struct Warp {
    base: ModuleBase<WarpState, MuonConfig>,
    pub wid: usize,
}

impl ModuleBehaviors for Warp {
    fn tick_one(&mut self) {
        { self.base().cycle += 1; }
    }

    fn reset(&mut self) {
        self.state().reg_file.iter_mut().for_each(|x| x.reset());
        self.state().csr_file.iter_mut().for_each(|x| x.reset());
    }
}

module!(Warp, WarpState, MuonConfig,
);

impl Warp {
    pub fn new(config: Arc<MuonConfig>) -> Warp {
        let num_lanes = config.num_lanes;
        let mut me = Warp {
            base: ModuleBase {
                state: WarpState {
                    stalled: false,
                    reg_file: (0..num_lanes).map(|i| RegFile::new(config.clone(), i)).collect(),
                    csr_file: (0..num_lanes).map(|i| CSRFile::new(config.clone(), i)).collect(),
                },
                ..ModuleBase::default()
            },
            wid: config.lane_config.warp_id,
        };
        me.init_conf(config);
        me
    }

    fn name(&self) -> String {
        format!("core {} warp {}", self.conf().lane_config.core_id, self.conf().lane_config.warp_id)
    }

    pub fn execute(&mut self, schedule: Schedule,
                   scheduler: &mut Scheduler, neutrino: &mut Neutrino) {
        info!("{} cycle={} fetch=0x{:08x}", self.name(), self.base.cycle, schedule.pc);

        let inst_data = *GMEM.write()
            .expect("gmem poisoned")
            .read::<8>(schedule.pc as usize)
            .expect("failed to fetch instruction");

        info!("{} decode=0x{:08x} end_stall={}", self.name(), schedule.pc, schedule.end_stall);
        self.base.state.stalled &= !schedule.end_stall;
        if self.base.state.stalled {
            info!("warp stalled");
            return;
        }

        self.state().csr_file.iter_mut().for_each(|c| {
            c.emu_access(0xcc3, schedule.active_warps);
            c.emu_access(0xcc4, schedule.mask);
        });

        ExecuteUnit::execute(DecodeUnit::decode(inst_data, schedule.pc),
             schedule.mask,
             self.wid,
             &mut self.base.state.reg_file.iter_mut().collect::<Vec<_>>(),
             &mut self.base.state.csr_file.iter_mut().collect::<Vec<_>>(),
             scheduler,
             neutrino,
        );

        // decode, execute, write back to register file
                // let rf = &self.lanes[lane_id].reg_file;
                // let decoded = self.lanes[lane_id].decode_unit.decode(inst_data, schedule.pc, rf);
                // let writeback = self.lanes[lane_id].execute_unit.execute(decoded);

                // let rf_mut = &mut self.lanes[lane_id].reg_file;

                // rf_mut.write_gpr(writeback.rd_addr, writeback.rd_data);


    }
}
