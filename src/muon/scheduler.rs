use std::collections::VecDeque;
use std::sync::Arc;
use log::info;
use crate::base::behavior::*;
use crate::base::module::{module, ModuleBase, IsModule};
use crate::base::port::{InputPort, OutputPort, Port};
use crate::muon::config::MuonConfig;
use crate::muon::isa::SFUType;
use crate::muon::warp::ScheduleWriteback;
use crate::utils::{BitMask, BitSlice};

#[derive(Debug, Default)]
pub struct IpdomEntry {
    pub pc: u32,
    pub tmask: u32,
}

#[derive(Debug, Default)]
pub struct SchedulerState {
    pub active_warps: u32,
    thread_masks: Vec<u32>,
    pub tohost: Option<u32>,
    pc: Vec<u32>,
    ipdom_stack: Vec<VecDeque<IpdomEntry>>,
    end_stall: Vec<bool>,
}

#[derive(Debug, Default, Clone)]
pub struct Schedule {
    pub pc: u32,
    pub mask: u32,
    pub active_warps: u32,
    pub end_stall: bool,
}

// instantiated per core
#[derive(Debug, Default)]
pub struct Scheduler {
    base: ModuleBase<SchedulerState, MuonConfig>,
    pub schedule: Vec<Option<Schedule>>,
    pub schedule_wb: Vec<Option<ScheduleWriteback>>,
}

impl Scheduler {
    pub fn new(config: Arc<MuonConfig>) -> Self {
        let num_warps = config.num_warps;
        info!("scheduler instantiated with {} warps!", num_warps);
        let mut me = Scheduler {
            base: ModuleBase::<SchedulerState, MuonConfig> {
                state: SchedulerState {
                    active_warps: 0,
                    thread_masks: vec![0u32; num_warps],
                    tohost: None,
                    pc: vec![0x80000000u32; num_warps],
                    ipdom_stack: (0..num_warps).map(|_| VecDeque::new()).collect(),
                    end_stall: vec![false; num_warps],
                },
                ..ModuleBase::default()
            },
            schedule: vec![None; num_warps],
            schedule_wb: vec![None; num_warps],
        };
        me.init_conf(config);
        me
    }

    pub fn spawn_warp(&mut self) {
        let all_ones = u32::MAX; // 0xFFFF
        self.state().thread_masks = [all_ones].repeat(self.conf().num_warps);
        self.base.state.active_warps = 1;
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_warps_retired(&self) -> bool {
        self.base.state.active_warps == 0
    }
}

module!(Scheduler, SchedulerState, MuonConfig,
);

impl ModuleBehaviors for Scheduler {
    fn tick_one(&mut self) {
        self.schedule_wb.iter_mut().enumerate().for_each(|(wid, writeback)| {
            if let Some(wb) = writeback {
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
                    self.base.state.pc[wid] = wb.first_inst.pc + 8; // flush
                    info!("processing writeback for {}: resetting next pc to 0x{:08x}", wid, wb.first_inst.pc + 8);
                    match sfu {
                        SFUType::TMC => {
                            let tmask = wb.first_inst.rs1;
                            info!("tmc value {}", tmask);
                            self.base.state.thread_masks[wid] = tmask;
                            if tmask == 0 {
                                self.base.state.active_warps.mut_bit(wid, false);
                            }
                        }
                        SFUType::WSPAWN => {
                            let start_pc = wb.first_inst.rs2;
                            info!("wspawn {} warps @pc={:08x}", wb.first_inst.rs1, start_pc);
                            for i in 0..wb.first_inst.rs1 as usize {
                                self.base.state.pc[i] = start_pc;
                                if !self.base.state.active_warps.bit(i) {
                                    self.base.state.thread_masks[i] = u32::MAX;
                                }
                                self.base.state.active_warps.mut_bit(i, true);
                            }
                            info!("new active warps: {:b}", self.base.state.active_warps);
                        }
                        SFUType::SPLIT => {
                            let invert = wb.first_inst.rs2_addr == 1;

                            // by default we use vx_split_n, which is called with the
                            // bits set high if NOT TAKING the then branch
                            let then_mask: Vec<_> = wb.insts.iter()
                                .map(|d| d.is_some_and(|dd| !dd.rs1.bit(0))).collect();
                            let else_mask: Vec<_> = wb.insts.iter()
                                .map(|d| d.is_some_and(|dd| dd.rs1.bit(0))).collect();
                            let then_mask_int = then_mask.to_u32();
                            let else_mask_int = else_mask.to_u32();

                            info!("split warp {}: then_mask={:08b}, else_mask={:08b}, invert={}",
                                wid, then_mask_int, else_mask_int, invert);

                            // NOTE: we should not need to push non-divergent branches
                            // to the ipdom stack if we write back divergence to RD
                            // NOTE: we are NOT writing back 1 to RD for divergent branches,
                            // which differs from vortex behavior for now.
                            // TODO: reevaluate
                            self.base.state.ipdom_stack[wid].push_back(IpdomEntry {
                                pc: 0u32,
                                tmask: then_mask_int | else_mask_int,
                            });

                            let diverges = (then_mask_int != 0) && (else_mask_int != 0);

                            if diverges {
                                self.base.state.ipdom_stack[wid].push_back(IpdomEntry {
                                    pc: wb.first_inst.pc + 8,
                                    tmask: if invert {
                                        else_mask_int
                                    } else {
                                        then_mask_int
                                    },
                                });
                                self.base.state.thread_masks[wid] = if invert {
                                    then_mask_int
                                } else {
                                    else_mask_int
                                };
                            }
                        }
                        SFUType::JOIN => {
                            let entry = self.base.state.ipdom_stack[wid].pop_back()
                                .expect("join without split");
                            info!("join warp {}: pc=0x{:08x}, tmask={:b}",
                                  wid, entry.pc, entry.tmask);
                            if (entry.pc > 0) {
                                self.base.state.pc[wid] = entry.pc;
                            }
                            self.base.state.thread_masks[wid] = entry.tmask;
                        }
                        SFUType::BAR => {
                            todo!()
                        }
                        SFUType::PRED => {
                            let invert = wb.first_inst.rd == 1;
                            // only stay active if (thread is active) AND (lsb of predicate is 1)
                            let pred_mask: Vec<_> = wb.insts.iter()
                                .map(|d| d.is_some_and(|dd| dd.rs1.bit(0) ^ invert))
                                .collect();
                            self.base.state.thread_masks[wid] &= pred_mask.to_u32();

                            // if all threads are not active, set thread mask to rs2 of warp leader
                            if self.base.state.thread_masks[wid] == 0 {
                                let tmask = wb.first_inst.rs2;
                                self.base.state.thread_masks[wid] = tmask;
                                if tmask == 0 {
                                    self.base.state.active_warps.mut_bit(wid, false);
                                }
                            }
                        }
                        SFUType::KILL => {
                            self.base.state.thread_masks[wid] = 0;
                            self.base.state.active_warps.mut_bit(wid, false);
                        }
                        SFUType::ECALL => {
                            let a0 = wb.first_inst.rs1;
                            self.base.state.tohost = Some(a0);
                            self.base.state.thread_masks[wid] = 0;
                            self.base.state.active_warps.mut_bit(wid, false);
                        }
                    }
                    self.base.state.end_stall[wid] = true;
                }
            }
        });

        self.schedule.iter_mut().enumerate().for_each(|(wid, warp)| {
            if self.base.state.active_warps.bit(wid) {
                let &pc = &self.base.state.pc[wid];
                *warp = Some(Schedule {
                    pc,
                    mask: self.base.state.thread_masks[wid],
                    active_warps: self.base.state.active_warps,
                    end_stall: self.base.state.end_stall[wid],
                });
                self.base.state.end_stall[wid] = false;
                *(&mut self.base.state.pc[wid]) += 8;
            } else {
                *warp = None;
            }
        });
    }

    fn reset(&mut self) {
        let all_ones = u32::MAX; // 0xFFFF
        self.state().thread_masks = [all_ones].repeat(self.conf().num_warps);
        self.base.state.active_warps = 0;
    }
}
