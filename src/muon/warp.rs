use crate::base::behavior::*;
use crate::base::mem::HasMemory;
use crate::base::module::{module, IsModule, ModuleBase};
use crate::muon::config::MuonConfig;
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, RegFile};
use crate::muon::execute::ExecuteUnit;
use crate::muon::scheduler::{Schedule, Scheduler};
use crate::sim::top::GMEM;
use crate::sim::log::Logger;
use crate::info;
use std::sync::Arc;
use crate::neutrino::neutrino::Neutrino;

#[derive(Debug, Default)]
pub struct WarpState {
    pub reg_file: Vec<RegFile>,
    pub csr_file: Vec<CSRFile>,
}

pub struct Warp {
    base: ModuleBase<WarpState, MuonConfig>,
    pub wid: usize,
    logger: Arc<Logger>,
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
    pub fn new(config: Arc<MuonConfig>, logger: &Arc<Logger>) -> Warp {
        let num_lanes = config.num_lanes;
        let mut me = Warp {
            base: ModuleBase {
                state: WarpState {
                    reg_file: (0..num_lanes).map(|i| RegFile::new(config.clone(), i)).collect(),
                    csr_file: (0..num_lanes).map(|i| CSRFile::new(config.clone(), i)).collect(),
                },
                ..ModuleBase::default()
            },
            wid: config.lane_config.warp_id,
            logger: logger.clone(),
        };
        me.init_conf(config);
        me
    }

    fn name(&self) -> String {
        format!("core {} warp {}", self.conf().lane_config.core_id, self.conf().lane_config.warp_id)
    }

    pub fn execute(&mut self, schedule: Schedule,
                   scheduler: &mut Scheduler, neutrino: &mut Neutrino) {
        info!(self.logger, "@t={} [{}] PC=0x{:08x}", self.base.cycle, self.name(), schedule.pc);

        let inst_data = *GMEM.write()
            .expect("gmem poisoned")
            .read::<8>(schedule.pc as usize)
            .expect("failed to fetch instruction");

        self.state().csr_file.iter_mut().for_each(|c| {
            c.emu_access(0xcc3, schedule.active_warps);
            c.emu_access(0xcc4, schedule.mask);
        });

        ExecuteUnit::execute(DecodeUnit::decode(inst_data, schedule.pc),
             self.conf().lane_config.core_id,
             self.wid,
             schedule.mask,
             &mut self.base.state.reg_file.iter_mut().collect::<Vec<_>>(),
             &mut self.base.state.csr_file.iter_mut().collect::<Vec<_>>(),
             scheduler,
             neutrino,
        );
    }
}
