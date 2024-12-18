use std::sync::Arc;
use log::info;
use crate::base::behavior::*;
use crate::base::behavior::Parameterizable;
use crate::base::component::{component, ComponentBase, IsComponent};
use crate::base::mem::{MemRequest, MemResponse};
use crate::base::port::{InputPort, OutputPort, Port};
use crate::builtin::queue::Queue;
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::csr::CSRFile;
use crate::muon::decode::{DecodeUnit, DecodedInst, RegFile};
use crate::muon::execute::{ExecuteUnit, Writeback};
use crate::muon::isa::{CSRType, SFUType};
use crate::muon::scheduler::ScheduleOut;
use crate::utils::BitSlice;

#[derive(Default)]
pub struct WarpState {
    pub stalled: bool,
}

#[derive(Clone, Default)]
pub struct FetchMetadata {
    pub mask: u32,
    pub pc: u32,
    pub active_warps: u32,
    pub end_stall: bool,
}

impl From<&ScheduleOut> for FetchMetadata {
    fn from(value: &ScheduleOut) -> Self {
        Self {
            pc: value.pc,
            mask: value.mask,
            active_warps: value.active_warps,
            end_stall: value.end_stall
        }
    }
}

#[derive(Default, Clone)]
pub struct ScheduleWriteback {
    pub insts: Vec<DecodedInst>,
    pub branch: Option<u32>,
    pub sfu: Option<SFUType>,
}

#[derive(Default)]
pub struct Warp {
    base: ComponentBase<WarpState, MuonConfig>,
    pub lanes: Vec<Lane>,

    pub fetch_queue: Queue<FetchMetadata, 4>,
    pub schedule: Port<InputPort, ScheduleOut>,

    pub imem_req: Port<OutputPort, MemRequest>,
    pub imem_resp: Port<InputPort, MemResponse>,

    pub schedule_wb: Port<OutputPort, ScheduleWriteback>,
}

impl ComponentBehaviors for Warp {
    fn tick_one(&mut self) {
        { self.base().cycle += 1; }
        // fetch
        if let Some(schedule) = self.schedule.get() {
            info!("warp {} fetch=0x{:08x}", self.conf().lane_config.warp_id, schedule.pc);
            if !self.imem_req.blocked() && self.fetch_queue.try_enq(&(&schedule).into()) {
                assert!(self.imem_req.read::<8>(schedule.pc as usize));
            }
        }
        // decode, execute, writeback
        if let Some(resp) = self.imem_resp.get() {
            let metadata = self.fetch_queue.try_deq().expect("fetch queue empty");

            info!("warp {} decode=0x{:08x} end_stall={}", self.conf().lane_config.warp_id, metadata.pc, metadata.end_stall);
            self.base.state.stalled &= !metadata.end_stall;
            if self.base.state.stalled {
                info!("warp stalled");
                return;
            }

            // decode, execute, write back to register file
            let inst_data: [u8; 8] = (*(resp.data.as_ref().unwrap().clone()))
                .try_into().expect("imem response is not 8 bytes");
            info!("cycle={}, pc={:08x}", self.base.cycle, metadata.pc);
            let writebacks: Vec<_> = (0..self.conf().num_lanes).map(|lane_id| {
                self.lanes[lane_id].csr_file.emu_access(0xcc3, metadata.active_warps);
                self.lanes[lane_id].csr_file.emu_access(0xcc4, metadata.mask);

                if !metadata.mask.bit(lane_id) {
                    return Writeback::default();
                }

                let rf = &self.lanes[lane_id].reg_file;
                let decoded = self.lanes[lane_id].decode_unit.decode(inst_data, metadata.pc, rf);
                let writeback = self.lanes[lane_id].execute_unit.execute(decoded.clone());

                let rf_mut = &mut self.lanes[lane_id].reg_file;

                // TODO: this will get clobbered for CSR insts, counts may be inaccurate
                rf_mut.write_gpr(writeback.rd_addr, writeback.rd_data);
                writeback
            }).collect();

            // update the scheduler
            writebacks[0].set_pc.map(|pc| {
                if pc == 0 {
                    println!("simulation has probably finished, main has returned");
                    std::process::exit(0);
                }
                self.state().stalled = true;
                ScheduleWriteback {
                    insts: writebacks.iter().map(|w| w.inst).collect(),
                    branch: Some(pc),
                    sfu: None,
                }
            }).or(writebacks[0].csr_type.map(|csr| {
                writebacks.iter().enumerate().for_each(|(lane_id, writeback)| {
                    let csr_mut = &mut self.lanes[lane_id].csr_file;
                    let csrr = (csr == CSRType::RS) && writeback.inst.rs1 == 0;
                    if [0xcc3, 0xcc4].contains(&writeback.rd_data) && !csrr {
                        panic!("unimplemented mask write using csr");
                    }
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
                });
                ScheduleWriteback {
                    insts: writebacks.iter().map(|w| w.inst).collect(),
                    branch: None,
                    sfu: None,
                }
            })).or(writebacks[0].sfu_type.map(|sfu| {
                self.state().stalled = true;
                ScheduleWriteback {
                    insts: writebacks.iter().map(|w| w.inst).collect(),
                    branch: None,
                    sfu: Some(sfu),
                }
            })).map(|wb| {
                assert!(self.schedule_wb.put(&wb));
            });
        }
    }

    fn reset(&mut self) {
        self.lanes.iter_mut().for_each(|lane| {
            lane.reg_file.reset();
            lane.execute_unit.reset();
        });
    }
}

component!(Warp, WarpState, MuonConfig,
    fn get_param_children(&mut self) -> Vec<&mut dyn Parameterizable<ConfigType=MuonConfig>> {
        let execute_units: Vec<_> = self.lanes.iter_mut().map(|l| {
            &mut l.execute_unit as &mut dyn Parameterizable<ConfigType=Self::ConfigType>
        }).collect();

        execute_units
    }
    
    fn get_children(&mut self) -> Vec<&mut dyn ComponentBehaviors> {
        todo!()
    }

    fn new(config: Arc<MuonConfig>) -> Warp {
        let num_lanes = config.num_lanes;
        info!("warp {} instantiated!", config.lane_config.warp_id);
        let mut me = Warp {
            base: ComponentBase::default(),
            lanes: (0..num_lanes).map(|lane_id| {
                let lane_config = Arc::new(MuonConfig {
                    lane_config: LaneConfig {
                        lane_id,
                        ..config.lane_config
                    },
                    ..*config
                });
                Lane {
                    reg_file: RegFile::new(lane_config.clone()),
                    csr_file: CSRFile::new(lane_config.clone()),
                    decode_unit: DecodeUnit,
                    execute_unit: ExecuteUnit::new(lane_config.clone()),
                }
            }).collect(),
            fetch_queue: Queue::new(Arc::new(())),
            schedule: Port::new(),
            imem_req: Port::new(),
            imem_resp: Port::new(),
            schedule_wb: Port::new(),
        };
        me.init_conf(config);
        me
    }
);

pub struct Lane {
    pub reg_file: RegFile,
    pub csr_file: CSRFile,
    pub decode_unit: DecodeUnit,
    pub execute_unit: ExecuteUnit,
}
