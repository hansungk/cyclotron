use std::sync::Arc;
use log::info;
use crate::base::behavior::*;
use crate::base::module::{module, ModuleBase, IsModule};
use crate::base::mem::{HasMemory, MemRequest, MemResponse};
use crate::base::port::{InputPort, OutputPort, Port};
use crate::builtin::queue::Queue;
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, DecodedInst, RegFile};
use crate::muon::execute::{ExecuteUnit, Writeback};
use crate::muon::isa::{CSRType, SFUType};
use crate::muon::scheduler::Schedule;
use crate::sim::top::GMEM;
use crate::utils::BitSlice;

#[derive(Debug, Default)]
pub struct WarpState {
    pub stalled: bool,
}

#[derive(Debug, Default, Clone)]
pub struct ScheduleWriteback {
    pub first_inst: DecodedInst,
    pub insts: Vec<Option<DecodedInst>>,
    pub branch: Option<u32>,
    pub sfu: Option<SFUType>,
}

#[derive(Debug, Default)]
pub struct Warp {
    base: ModuleBase<WarpState, MuonConfig>,
    pub lanes: Vec<Lane>,

    pub schedule: Option<Schedule>,
    pub schedule_wb: Option<ScheduleWriteback>,
}

impl ModuleBehaviors for Warp {
    fn tick_one(&mut self) {
        { self.base().cycle += 1; }
        // fetch
        if let Some(schedule) = &self.schedule {
            info!("warp {} fetch=0x{:08x}", self.conf().lane_config.warp_id, schedule.pc);

            let inst_data = *GMEM.write()
                .expect("gmem poisoned")
                .read::<8>(schedule.pc as usize)
                .expect("failed to fetch instruction");

            info!("warp {} decode=0x{:08x} end_stall={}", self.conf().lane_config.warp_id, schedule.pc, schedule.end_stall);
            self.base.state.stalled &= !schedule.end_stall;
            if self.base.state.stalled {
                info!("warp stalled");
                return;
            }

            // decode, execute, write back to register file
            // let inst_data: [u8; 8] = (*(resp.data.as_ref().unwrap().clone()))
            //     .try_into().expect("imem response is not 8 bytes");
            info!("cycle={}, pc={:08x}", self.base.cycle, schedule.pc);
            let writebacks: Vec<_> = (0..self.conf().num_lanes).map(|lane_id| {
                self.lanes[lane_id].csr_file.emu_access(0xcc3, schedule.active_warps);
                self.lanes[lane_id].csr_file.emu_access(0xcc4, schedule.mask);

                if !schedule.mask.bit(lane_id) {
                    return None;
                }

                let rf = &self.lanes[lane_id].reg_file;
                let decoded = self.lanes[lane_id].decode_unit.decode(inst_data, schedule.pc, rf);
                let writeback = self.lanes[lane_id].execute_unit.execute(decoded);

                let rf_mut = &mut self.lanes[lane_id].reg_file;

                rf_mut.write_gpr(writeback.rd_addr, writeback.rd_data);
                Some(writeback)
            }).collect();

            let first_wb = writebacks.iter().find(|&wb| wb.is_some()).unwrap().as_ref().unwrap();
            let wb_insts = writebacks.iter().map(|w| w.as_ref().and_then(|ww| Some(ww.inst))).collect();
            let base_sched_wb = ScheduleWriteback {
                first_inst: first_wb.inst,
                insts: wb_insts,
                branch: None,
                sfu: None
            };
            // update the scheduler
            self.schedule_wb = first_wb.set_pc.map(|pc| {
                self.state().stalled = true;
                ScheduleWriteback {
                    branch: Some(pc),
                    ..base_sched_wb.clone()
                }
            }).or(first_wb.csr_type.map(|csr| {
                writebacks.iter().enumerate().for_each(|(lane_id, writeback_opt)| {
                    if let Some(writeback) = writeback_opt {
                        let csr_mut = &mut self.lanes[lane_id].csr_file;
                        let csrr = (csr == CSRType::RS) && writeback.inst.rs1 == 0;
                        if [0xcc3, 0xcc4].contains(&writeback.rd_data) && !csrr {
                            panic!("unimplemented mask write using csr");
                        }
                        info!("csr read address {}", writeback.rd_data);
                        let old_val = csr_mut.user_access(writeback.rd_data, match csr {
                            CSRType::RW | CSRType::RS | CSRType::RC => {
                                writeback.inst.rs1
                            }
                            CSRType::RWI | CSRType::RSI | CSRType::RCI => {
                                writeback.inst.imm8 as u32
                            }
                        }, csr);
                        let rf_mut = &mut self.lanes[lane_id].reg_file;
                        rf_mut.write_gpr(writeback.rd_addr, old_val);
                        info!("csr read value {}", old_val);
                    }
                });
                base_sched_wb.clone()
            })).or(first_wb.sfu_type.map(|sfu| {
                self.state().stalled = true;
                ScheduleWriteback {
                    sfu: Some(sfu),
                    ..base_sched_wb.clone()
                }
            }));
        }
    }

    fn reset(&mut self) {
        self.lanes.iter_mut().for_each(|lane| {
            lane.reg_file.reset();
        });
    }
}

module!(Warp, WarpState, MuonConfig,
);

impl Warp {
    pub fn new(config: Arc<MuonConfig>) -> Warp {
        let num_lanes = config.num_lanes;
        let mut me = Warp {
            base: ModuleBase::default(),
            lanes: (0..num_lanes).map(|lane_id| {
                let lane_config = Arc::new(MuonConfig {
                    lane_config: LaneConfig {
                        lane_id,
                        ..config.lane_config
                    },
                    ..*config
                });
                Lane {
                    reg_file: RegFile::new(Arc::clone(&lane_config)),
                    csr_file: CSRFile::new(Arc::clone(&lane_config)),
                    decode_unit: DecodeUnit,
                    execute_unit: ExecuteUnit::new(Arc::clone(&lane_config)),
                }
            }).collect(),
            schedule: None,
            schedule_wb: None,
        };
        me.init_conf(config);
        me
    }
}

#[derive(Debug)]
pub struct Lane {
    pub reg_file: RegFile,
    pub csr_file: CSRFile,
    pub decode_unit: DecodeUnit,
    pub execute_unit: ExecuteUnit,
}
