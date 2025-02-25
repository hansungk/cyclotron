use std::sync::Arc;
use log::info;
use crate::base::behavior::*;
use crate::base::component::{component, ComponentBase, IsComponent};
use crate::base::port::{InputPort, OutputPort, Port};
use crate::muon::config::MuonConfig;
use crate::muon::isa::SFUType;
use crate::muon::warp::ScheduleWriteback;
use crate::utils::{BitSlice};

#[derive(Default)]
pub struct SchedulerState {
    pub active_warps: u32,
    thread_masks: Vec<u32>,
    pc: Vec<u32>,
    _ipdom_stack: Vec<u32>,
    end_stall: Vec<bool>,
}

#[derive(Default, Clone)]
pub struct ScheduleOut {
    pub pc: u32,
    pub mask: u32,
    pub active_warps: u32,
    pub end_stall: bool,
}

// instantiated per core
#[derive(Default)]
pub struct Scheduler {
    base: ComponentBase<SchedulerState, MuonConfig>,
    pub schedule: Vec<Port<OutputPort, ScheduleOut>>,
    pub schedule_wb: Vec<Port<InputPort, ScheduleWriteback>>,
}

impl ComponentBehaviors for Scheduler {
    fn tick_one(&mut self) {

        self.schedule_wb.iter_mut().enumerate().for_each(|(wid, port)| {
            if let Some(wb) = port.get() {
                if let Some(target_pc) = wb.branch {
                    self.base.state.pc[wid] = target_pc;
                    self.base.state.end_stall[wid] = true;
                    if target_pc == 0 { // returned from main
                        self.base.state.thread_masks[wid] = 0;
                        self.base.state.active_warps.mut_bit(wid, false);
                    }
                }
                if let Some(sfu) = wb.sfu {
                    // for warp-wide operations, we take lane 0 to be the truth
                    self.base.state.pc[wid] = wb.insts[0].pc + 8; // flush
                    info!("resetting next pc to 0x{:08x}", wb.insts[0].pc + 8);
                    match sfu {
                        SFUType::TMC => {
                            let tmask = wb.insts[0].rs1;
                            info!("tmc value {}", tmask);
                            self.base.state.thread_masks[wid] = tmask;
                            if tmask == 0 {
                                self.base.state.active_warps.mut_bit(wid, false);
                            }
                        }
                        SFUType::WSPAWN => {
                            let start_pc = wb.insts[0].rs2;
                            info!("wspawn {} warps @pc={:08x}", wb.insts[0].rs1, start_pc);
                            for i in 0..wb.insts[0].rs1 as usize {
                                if !self.base.state.active_warps.bit(i) {
                                    self.base.state.pc[i] = start_pc;
                                }
                                self.base.state.active_warps.mut_bit(i, true);
                            }
                            info!("new active warps: {:b}", self.base.state.active_warps);
                        }
                        SFUType::SPLIT => {
                            let then_mask: Vec<_> = wb.insts.iter().map(|d| d.rs1.bit(0)).collect();
                            let else_mask: Vec<_> = then_mask.iter().map(|d| !d).collect();
                            let _sup = else_mask;
                            todo!()
                        }
                        SFUType::JOIN => {
                            todo!()
                        }
                        SFUType::BAR => {
                            todo!()
                        }
                        SFUType::PRED => {
                            todo!()
                        }
                        SFUType::KILL => {
                            self.base.state.thread_masks[wid] = 0;
                            self.base.state.active_warps.mut_bit(wid, false);
                        }
                    }
                    self.base.state.end_stall[wid] = true;
                }
            }
        });
        
        if self.base.state.active_warps == 0 {
            info!("simulation has ended");
        }

        self.schedule.iter_mut().enumerate().for_each(|(wid, port)| {
            let ready = self.base.state.active_warps.bit(wid) && !port.blocked();
            if ready {
                let &pc = &self.base.state.pc[wid];
                port.put(&ScheduleOut {
                    pc,
                    mask: self.base.state.thread_masks[wid],
                    active_warps: self.base.state.active_warps,
                    end_stall: self.base.state.end_stall[wid],
                });
                self.base.state.end_stall[wid] = false;
                *(&mut self.base.state.pc[wid]) += 8;
            }
        });
    }

    fn reset(&mut self) {
        let num_lanes = (&self.conf().num_lanes).clone();
        let tmask = ((1u64 << num_lanes) - 1u64) as u32;
        self.state().thread_masks = [tmask].repeat(num_lanes);
        self.base.state.active_warps = 1;
    }
}

component!(Scheduler, SchedulerState, MuonConfig,
    fn new(config: Arc<MuonConfig>) -> Scheduler {
        let num_warps = config.num_warps;
        info!("scheduler instantiated with {} warps!", num_warps);
        let mut me = Scheduler {
            base: ComponentBase::<SchedulerState, MuonConfig> {
                state: SchedulerState {
                    active_warps: 0,
                    thread_masks: vec![0u32; num_warps],
                    pc: vec![0x80000000u32; num_warps],
                    _ipdom_stack: vec![0; num_warps],
                    end_stall: vec![false; num_warps],
                },
                ..ComponentBase::default()
            },
            schedule: vec![Port::new(); num_warps],
            schedule_wb: vec![Port::new(); num_warps],
        };
        me.init_conf(config);
        me
    }
);
