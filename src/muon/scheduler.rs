use std::collections::VecDeque;
use std::iter::{once, repeat};
use std::sync::Arc;
use log::info;
use crate::base::behavior::*;
use crate::base::module::{module, ModuleBase, IsModule};
use crate::muon::config::MuonConfig;
use crate::muon::decode::IssuedInst;
use crate::muon::execute::SFUType;
use crate::utils::{BitMask, BitSlice};

#[derive(Debug, Default)]
pub struct IpdomEntry {
    pub pc: u32,
    pub tmask: u32,
}

#[derive(Debug, Default)]
pub struct SchedulerState {
    /// differentiates between the initial and the final state, in both of which no active warps
    /// exist
    pub started: bool,
    pub active_warps: u32,
    pub stalled_warps: u32,
    thread_masks: Vec<u32>,
    pub tohost: Option<u32>,
    pc: Vec<u32>,
    ipdom_stack: Vec<VecDeque<IpdomEntry>>,
}

/// Per-warp info of which instruction to fetch next
#[derive(Debug, Default, Clone, Copy)]
pub struct Schedule {
    pub pc: u32,
    pub mask: u32,
    pub active_warps: u32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SchedulerWriteback {
    pub pc: Option<u32>,
    pub tmask: Option<u32>,
    pub wspawn_pc_count: Option<(u32, u32)>,
    pub tohost: Option<u32>,
    // TODO: ipdom stack entries
}

// instantiated per core
#[derive(Debug, Default)]
pub struct Scheduler {
    base: ModuleBase<SchedulerState, MuonConfig>,
    cid: usize,
}

impl Scheduler {
    pub fn new(config: Arc<MuonConfig>, cid: usize) -> Self {
        let num_warps = config.num_warps;
        info!("scheduler instantiated with {} warps!", num_warps);
        info!("start_pc: {}", config.start_pc);
        let mut me = Scheduler {
            base: ModuleBase::<SchedulerState, MuonConfig> {
                state: SchedulerState {
                    started: false,
                    active_warps: 0,
                    stalled_warps: 0,
                    thread_masks: vec![0u32; num_warps],
                    tohost: None,
                    pc: vec![config.start_pc; num_warps],
                    ipdom_stack: (0..num_warps).map(|_| VecDeque::new()).collect(),
                },
                ..ModuleBase::default()
            },
            cid
        };
        me.init_conf(config);
        me
    }

    pub fn spawn_single_warp(&mut self) {
        let all_ones = u32::MAX; // 0xFFFF
        self.state_mut().started = true;
        self.state_mut().thread_masks = [all_ones].repeat(self.conf().num_warps);
        self.state_mut().active_warps = 1;
    }

    pub fn spawn_n_warps(&mut self, pc: u32, thread_idxs: &Vec<Vec<(u32, u32, u32)>>) {
        let all_ones = u32::MAX; // 0xFFFF
        let last_warp_mask = thread_idxs.last().expect("last warp is empty").iter().enumerate().fold(0, |mask, (i, _)| mask | 1 << i);
        let num_warps = thread_idxs.len();
        let disabled_warps = self.conf().num_warps - num_warps;
        assert!(disabled_warps <= self.conf().num_warps);

        self.state_mut().started = true;
        self.state_mut().thread_masks = repeat(all_ones).take(num_warps - 1).chain(once(last_warp_mask)).chain(repeat(0).take(disabled_warps)).collect();
        self.state_mut().pc = [pc].repeat(num_warps);
        self.base.state.active_warps = num_warps as u32;
    }

    pub fn take_branch(&mut self, wid: usize, target_pc: u32) -> SchedulerWriteback {
        self.base.state.pc[wid] = target_pc;
        if target_pc == 0 { // returned from main
            self.base.state.thread_masks[wid] = 0;
            self.base.state.active_warps.mut_bit(wid, false);
        }
        SchedulerWriteback {
            pc: Some(target_pc),
            ..SchedulerWriteback::default()
        }
    }

    pub fn sfu(&mut self, wid: usize, first_lid: usize, sfu: SFUType,
               decoded_inst: &IssuedInst, rs1: Vec<u32>, rs2: Vec<u32>) -> SchedulerWriteback {
        // for warp-wide operations, we take lane 0 to be the truth
        self.base.state.pc[wid] = decoded_inst.pc + 8; // flush
        info!("processing writeback for {}: resetting next pc to 0x{:08x}", wid, decoded_inst.pc + 8);
        match sfu {
            SFUType::TMC => {
                let tmask = rs1[first_lid];
                info!("tmc value {}", tmask);
                self.base.state.thread_masks[wid] = tmask;
                if tmask == 0 {
                    self.base.state.active_warps.mut_bit(wid, false);
                }
                SchedulerWriteback {
                    tmask: Some(tmask),
                    ..SchedulerWriteback::default()
                }
            }
            SFUType::WSPAWN => {
                let start_pc = rs2[first_lid];
                let count = rs1[first_lid];
                info!("wspawn {} warps @pc={:08x}", rs1[first_lid], start_pc);
                for i in 1..count as usize {
                    self.base.state.pc[i] = start_pc;
                    if !self.base.state.active_warps.bit(i) {
                        self.base.state.thread_masks[i] = u32::MAX;
                    }
                    self.base.state.active_warps.mut_bit(i, true);
                }
                info!("new active warps: {:b}", self.base.state.active_warps);
                SchedulerWriteback {
                    wspawn_pc_count: Some((start_pc, count)),
                    ..SchedulerWriteback::default()
                }
            }
            SFUType::SPLIT => {
                // deviation from vortex behavior: instead of checking for 1,
                // check for nonzero. works with renaming front end
                let invert = decoded_inst.rs2_addr != 0;
                // by default, we use vx_split_n, which is called with the
                // bits set high if NOT TAKING the then branch
                let then_mask = rs1.iter().map(|r| !r.bit(0)).collect::<Vec<_>>().to_u32()
                    & self.state().thread_masks[wid];
                let else_mask = rs1.iter().map(|r| r.bit(0)).collect::<Vec<_>>().to_u32()
                    & self.state().thread_masks[wid];
                // let then_mask: Vec<_> = wb.insts.iter()
                //     .map(|d| d.is_some_and(|dd| !dd.rs1.bit(0))).collect();
                // let else_mask: Vec<_> = wb.insts.iter()
                //     .map(|d| d.is_some_and(|dd| dd.rs1.bit(0))).collect();
                // let then_mask_int = then_mask.to_u32();
                // let else_mask_int = else_mask.to_u32();

                info!("split warp {}: then_mask={:08b}, else_mask={:08b}, invert={}",
                    wid, then_mask, else_mask, invert);

                // NOTE: we should not need to push non-divergent branches
                // to the ipdom stack if we write back divergence to RD
                // NOTE: we are NOT writing back 1 to RD for divergent branches,
                // which differs from vortex behavior for now.
                // TODO: reevaluate
                self.base.state.ipdom_stack[wid].push_back(IpdomEntry {
                    pc: 0u32,
                    tmask: then_mask | else_mask,
                });

                let diverges = (then_mask != 0) && (else_mask != 0);

                if diverges {
                    self.base.state.ipdom_stack[wid].push_back(IpdomEntry {
                        pc: decoded_inst.pc + 8,
                        tmask: if invert {
                            else_mask
                        } else {
                            then_mask
                        },
                    });
                    self.base.state.thread_masks[wid] = if invert {
                        then_mask
                    } else {
                        else_mask
                    };
                }
                SchedulerWriteback {
                    // TODO
                    ..SchedulerWriteback::default()
                }
            }
            SFUType::JOIN => {
                let entry = self.base.state.ipdom_stack[wid].pop_back()
                    .expect("join without split");
                info!("join warp {}: pc=0x{:08x}, tmask={:b}",
                      wid, entry.pc, entry.tmask);
                if entry.pc > 0 {
                    self.base.state.pc[wid] = entry.pc;
                }
                self.base.state.thread_masks[wid] = entry.tmask;
                // join is not visible to cyclotron in cosim
                SchedulerWriteback::default()
            }
            SFUType::BAR => {
                panic!("muon does not support vx_bar anymore, use neutrino insts")
            }
            SFUType::PRED => {
                let invert = decoded_inst.rd_addr != 0;
                // only stay active if (thread is active) AND (lsb of predicate is 1)
                // let pred_mask: Vec<_> = wb.insts.iter()
                //     .map(|d| d.is_some_and(|dd| dd.rs1.bit(0) ^ invert))
                //     .collect();
                let pred_mask = rs1.iter()
                    .map(|r| r.bit(0) ^ invert)
                    .collect::<Vec<_>>().to_u32();
                self.base.state.thread_masks[wid] &= pred_mask;

                // if all threads are not active, set thread mask to rs2 of warp leader
                if self.base.state.thread_masks[wid] == 0 {
                    let tmask = rs2[first_lid];
                    self.base.state.thread_masks[wid] = tmask;
                    if tmask == 0 {
                        self.base.state.active_warps.mut_bit(wid, false);
                    }
                }
                SchedulerWriteback {
                    tmask: Some(self.base.state.thread_masks[wid]),
                    ..SchedulerWriteback::default()
                }
            }
            SFUType::KILL => {
                self.base.state.thread_masks[wid] = 0;
                self.base.state.active_warps.mut_bit(wid, false);
                SchedulerWriteback {
                    tmask: Some(0),
                    ..SchedulerWriteback::default()
                }
            }
            SFUType::ECALL => {
                let a0 = rs1[first_lid];
                self.base.state.tohost = Some(a0);
                self.base.state.thread_masks[wid] = 0;
                self.base.state.active_warps.mut_bit(wid, false);
                if (a0 == 0) {
                    println!("test passed!")
                } else {
                    println!("test failed with tohost={}", a0);
                }
                // might want a tohost writeback here
                SchedulerWriteback {
                    tohost: Some(a0),
                    ..SchedulerWriteback::default()
                }
            }
        }
    }

    pub fn get_schedule(&mut self, wid: usize) -> Option<Schedule> {
        (self.state().active_warps.bit(wid) && !self.state().stalled_warps.bit(wid)).then(|| {
            let pc = self.state().pc[wid];
            let sched = Schedule {
                pc,
                mask: self.base.state.thread_masks[wid],
                active_warps: self.base.state.active_warps, // for csr writing
            };
            self.state_mut().pc[wid] = pc.wrapping_add(8);
            sched
        })
    }

    pub fn neutrino_stall(&mut self, stalls: Vec<bool>) {
        self.state_mut().stalled_warps = stalls.to_u32();
    }

    // TODO: This should differentiate between different threadblocks.
    pub fn all_warps_retired(&self) -> bool {
        self.state().started && (self.state().active_warps == 0)
    }
}

module!(Scheduler, SchedulerState, MuonConfig,
);

impl ModuleBehaviors for Scheduler {
    fn tick_one(&mut self) {
        self.base.cycle += 1;
        info!("core {} active warps {:08b} stalled warps {:08b}",
              self.cid, self.base.state.active_warps, self.base.state.stalled_warps);
    }

    fn reset(&mut self) {
        let all_ones = u32::MAX; // 0xFFFF
        self.state_mut().thread_masks = [all_ones].repeat(self.conf().num_warps);
        self.base.state.active_warps = 0;
        self.base.state.stalled_warps = 0;
    }
}
