use log::{debug, info};
use std::sync::Arc;
use crate::base::behavior::{ModuleBehaviors, Parameterizable};
use crate::base::module::IsModule;
use crate::base::module::{module, ModuleBase};
use crate::muon::decode::{DecodedInst, RegFile};
use crate::muon::execute::{InstDef, Opcode};
use crate::muon::scheduler::Scheduler;
use crate::neutrino::config::NeutrinoConfig;
use crate::neutrino::scoreboard::{NeutrinoCmd, NeutrinoCmdType, NeutrinoPartMode, NeutrinoRetMode, Scoreboard};
use crate::print_and_unwrap;
use crate::utils::BitSlice;

#[derive(Default)]
pub struct NeutrinoState {
}

pub struct Neutrino {
    base: ModuleBase<NeutrinoState, NeutrinoConfig>,
    scoreboard: Scoreboard,
}

module!(Neutrino, NeutrinoState, NeutrinoConfig,);

impl ModuleBehaviors for Neutrino {
    fn tick_one(&mut self) {
        self.scoreboard.tick_one();
        self.base.cycle += 1;
    }

    fn reset(&mut self) {
        self.scoreboard.reset();
    }
}

impl Neutrino {
    pub fn new(config: Arc<NeutrinoConfig>) -> Self {
        let mut me = Neutrino {
            base: ModuleBase::<NeutrinoState, NeutrinoConfig> {
                state: NeutrinoState {},
                ..ModuleBase::default()
            },
            scoreboard: Scoreboard::new(config.clone()),
        };
        me.init_conf(config.clone());
        me
    }
    
    pub fn execute(&mut self, decoded: &DecodedInst,
                   cid: usize, wid: usize, tmask: u32,
                   rf: &mut RegFile) {

        // decode neutrino instruction
        let imp = match decoded.opcode {
            Opcode::NU_INVOKE => InstDef("nu.invoke", NeutrinoCmdType::Invoke),
            Opcode::NU_PAYLOAD => InstDef("nu.payload", NeutrinoCmdType::Payload),
            Opcode::NU_COMPLETE => InstDef("nu.complete", NeutrinoCmdType::Complete),
            _ => panic!("unimplemented"),
        };
        let cmd_type = print_and_unwrap!(imp);
        let cmd = match cmd_type {
            NeutrinoCmdType::Invoke => {
                NeutrinoCmd {
                    cmd_type,
                    job_id: None,
                    task: rf.read_gpr(decoded.rs1_addr),
                    deps: [decoded.rs2_addr, decoded.rs3_addr, decoded.rs4_addr].iter()
                        .filter_map(|&addr| (addr != 0).then(|| rf.read_gpr(addr)))
                        .filter_map(|r| (r != 0).then(|| self.scoreboard.job_id_from_u32(r)))
                        .collect::<Vec<_>>(),
                    ret_mode: match decoded.raw.sel(19, 18) & 3 {
                        0 => NeutrinoRetMode::Immediate,
                        1 => NeutrinoRetMode::Hardware,
                        2 => NeutrinoRetMode::Manual,
                        _ => panic!("unimplemented"),
                    },
                    part_mode: match decoded.raw.sel(53, 52) & 3 {
                        0 => NeutrinoPartMode::Threads,
                        1 => NeutrinoPartMode::Warps,
                        2 => NeutrinoPartMode::ThreadBlocks,
                        _ => panic!("unimplemented"),
                    },
                    num_elems: decoded.raw.sel(59, 54) as u32 + 1,
                    sync: decoded.raw.bit(17),
                    tmask
                }
            }
            NeutrinoCmdType::Payload => { todo!() }
            NeutrinoCmdType::Complete => { todo!() }
        };

        // put it in the scoreboard and get an id back
        // TODO: are writing back for non-invokes?
        let job_id = self.scoreboard.arrive(&cmd, cid, wid);
        let job_id_u32 = self.scoreboard.u32_from_job_id(job_id);
        rf.write_gpr(decoded.rd_addr, job_id_u32);

        info!("{} has job id {} written to x{}", cmd, job_id_u32, decoded.rd_addr);
    }

    pub fn update(&mut self, schedulers: &mut Vec<&mut Scheduler>) {
        // TODO: some steps can probably be skipped if no changes in state
        // find all ready jobs
        let ready_jobs = self.scoreboard.find_ready_jobs();
        // dispatch jobs
        ready_jobs.iter().for_each(|job_id| {
            self.scoreboard.dispatch(*job_id);
        });
        // inform scheduler of stalls
        let stalls = self.scoreboard.get_stalls();
        schedulers.iter_mut().zip(stalls.iter()).for_each(|(scheduler, stall)| {
            scheduler.neutrino_stall(stall.iter().map(|x| *x > 0).collect());
        });
    }
}
