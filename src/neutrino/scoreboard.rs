use std::cmp::PartialEq;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use log::error;
use crate::base::module::IsModule;
use crate::base::behavior::{ModuleBehaviors, Parameterizable};
use crate::base::module::{module, ModuleBase};
use crate::neutrino::config::NeutrinoConfig;
use crate::neutrino::counters::Counters;
use crate::neutrino::scoreboard::JobStatus::NotStarted;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobID {
    pub(crate) task_id: u32,
    pub(crate) counter: u32,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum RetMode {
    Immediate,
    Hardware,
    Manual,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum PartMode {
    Threads,
    Warps,
    ThreadBlocks,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum JobStatus {
    NotStarted,
    RunningOrFinished,
    Finished,
}

pub struct ScoreboardEntry {
    deps: Vec<JobID>,
    ret_mode: RetMode,
    part_mode: PartMode,
    sync: bool,
    num_elems: u32,
    arrived_mask: Vec<Vec<u64>>, // mask[wid] = thread mask
    dispatched: bool,
}

#[derive(Default)]
pub struct ScoreboardState {
    entries: HashMap<JobID, ScoreboardEntry>,
}

#[derive(Copy, Clone)]
pub enum NeutrinoCmdType {
    Invoke,
    Payload,
    Complete,
}

#[derive(Clone)]
pub struct NeutrinoCmd {
    cmd_type: NeutrinoCmdType,
    job_id: Option<JobID>,
    task: u32,
    deps: Vec<JobID>,
    ret_mode: RetMode,
    part_mode: PartMode,
    num_elems: u32,
    sync: bool,
    tmask: u64,
}

#[derive(Clone)]
pub struct NeutrinoSchedulerControl {
    stall: Vec<bool>,
}

pub struct Scoreboard {
    base: ModuleBase<ScoreboardState, NeutrinoConfig>,
    counters: Counters,
    cmd_in: Vec<Vec<Option<NeutrinoCmd>>>,
    schedule_out: Vec<NeutrinoSchedulerControl>, // one struct per core
}

module!(Scoreboard, ScoreboardState, NeutrinoConfig,);

impl ModuleBehaviors for Scoreboard {
    fn tick_one(&mut self) {
        // assume inputs have arrived
        self.process_inputs();
        // find all ready jobs
        let ready_jobs = self.find_ready_jobs();
        // dispatch jobs
        ready_jobs.iter().for_each(|job_id| {
            self.dispatch(*job_id);
        });
        // update stalls
        self.update_stalls();
    }
}

impl Scoreboard {
    pub fn new(config: Arc<NeutrinoConfig>) -> Self {
        let muon_config = config.muon_config;
        let counters = Counters::new(config.clone());
        let mut me = Scoreboard {
            base: ModuleBase::<ScoreboardState, NeutrinoConfig> {
                state: ScoreboardState {
                    entries: HashMap::new(),
                },
                ..ModuleBase::default()
            },
            counters,
            cmd_in: vec![vec![None; muon_config.num_warps]; muon_config.num_cores],
            schedule_out: vec![NeutrinoSchedulerControl {
                stall: vec![false; config.muon_config.num_cores]
            }; muon_config.num_cores],
        };
        me.init_conf(config);
        me
    }

    pub fn arrive(&mut self, cmd: NeutrinoCmd, core_id: usize, warp_id: usize) {
        assert!(core_id < self.conf().muon_config.num_cores, "core id out of range");
        assert!(warp_id < self.conf().muon_config.num_warps, "warp id out of range");
        assert!(cmd.task > 0, "task 0 is reserved");
        let cmd_in = &mut self.cmd_in[core_id][warp_id];
        if cmd_in.is_none() {
            *cmd_in = Some(cmd);
        } else {
            error!("command already arrived");
        }
    }

    pub fn verify(&self, job_id: JobID) {
        assert!(self.base.state.entries.contains_key(&job_id),
            "job {} not found", job_id.task_id)
    }

    pub fn process_inputs(&mut self) {
        let num_cores = self.conf().muon_config.num_cores;
        let num_warps = self.conf().muon_config.num_warps;
        self.cmd_in.iter().enumerate().for_each(|(cid, warps)| {
            warps.iter().enumerate().for_each(|(wid, cmd)| {
                if let Some(cmd) = cmd {
                    match cmd.cmd_type {
                        NeutrinoCmdType::Invoke => {
                            // first try to find existing non-dispatched entry
                            let existing_job_id = self.counters.peek(cmd.task);
                            let entry_opt = self.base.state.entries.get_mut(&existing_job_id);
                            let use_existing = entry_opt.as_ref().is_some_and(|e| !e.dispatched);
                            if use_existing {
                                // TODO: warn if existing parameters mismatch incoming
                                let entry = entry_opt.unwrap();
                                entry.arrived_mask[cid][wid] |= cmd.tmask
                            } else {
                                // either entry doesn't exist or is already dispatched
                                // so we make a new entry
                                let job_id = self.counters.take(cmd.task);
                                let mut entry = ScoreboardEntry {
                                    deps: cmd.deps.clone(),
                                    ret_mode: cmd.ret_mode,
                                    part_mode: cmd.part_mode,
                                    sync: cmd.sync,
                                    num_elems: cmd.num_elems,
                                    arrived_mask: vec![vec![0u64; num_warps]; num_cores],
                                    dispatched: false,
                                };
                                entry.arrived_mask[cid][wid] = cmd.tmask;
                                self.base.state.entries.insert(job_id, entry);
                            }
                        }
                        NeutrinoCmdType::Payload => {
                            // Handle payload command
                            todo!()
                        }
                        NeutrinoCmdType::Complete => {
                            // Handle complete command
                            todo!()
                        }
                    }
                }
            });
        });
    }

    pub fn is_ready(&self, job_id: JobID) -> bool {
        self.verify(job_id.clone());
        let entry = self.base.state.entries.get(&job_id).unwrap();
        let arrived = &entry.arrived_mask;
        let num_threads: u32 = arrived.iter().flat_map(|x| {
            x.iter().map(|y| y.count_ones())
        }).sum();
        // any thread in a warp means warp has arrived
        let num_warps: u32 = arrived.iter().flat_map(|x| {
            x.iter().filter(|y| **y != 0u64)
        }).collect::<Vec<_>>().len() as u32;

        let participation_satisfied  = (match entry.part_mode {
            PartMode::Threads => num_threads,
            PartMode::Warps => num_warps,
            PartMode::ThreadBlocks => todo!(),
        }) >= entry.num_elems;

        let dependencies_satisfied = entry.deps.iter().map(|dep| {
            (dep.task_id == 0) || // task 0 is reserved for no dependency
                (!self.base.state.entries.contains_key(dep) &&
                (self.counters.check(*dep) != NotStarted))
        }).all(|x| x);

        // if either the previous job doesn't exist or is dispatched, then it's in order
        let order_satisfied = self.base.state.entries.get(&JobID {
            task_id: job_id.task_id,
            counter: self.counters.pred(job_id.counter),
        }).is_none_or(|e| e.dispatched);

        participation_satisfied && dependencies_satisfied &&
            (!self.conf().in_order_issue || order_satisfied)
    }

    pub fn find_ready_jobs(&mut self) -> Vec<JobID> {
        self.base.state.entries.iter().filter_map(|(job_id, _entry)| {
            self.is_ready(*job_id).then_some(job_id.clone())
        }).collect()
    }

    pub fn dispatch(&mut self, job_id: JobID) {
        self.verify(job_id);
        assert!(self.is_ready(job_id), "job not ready");
        let entry = self.base.state.entries.get_mut(&job_id).unwrap();
        assert!(!entry.dispatched, "job already dispatched");
        entry.dispatched = true;

        if job_id.task_id < 8 { // barriers
            // do nothing
        } else {
            panic!("task id {} not implemented", job_id.task_id)
        }

        if entry.ret_mode == RetMode::Immediate {
            self.complete(job_id)
        }
    }

    pub fn complete(&mut self, job_id: JobID) {
        assert!(self.is_ready(job_id));
        let entry = self.base.state.entries.get(&job_id).unwrap();
        assert!(entry.dispatched);
        self.base.state.entries.remove_entry(&job_id);
    }

    pub fn update_stalls(&mut self) {
        // find all sync, undispatched jobs and OR their masks
        let mut stalls = vec![
            vec![0u64; self.conf().muon_config.num_warps];
            self.conf().muon_config.num_cores];
        let arrived_masks: Vec<Vec<Vec<u64>>> = self.base.state.entries.iter().filter_map(|e| {
            e.1.sync.then_some(e.1.arrived_mask.clone())
        }).collect::<Vec<_>>();
        arrived_masks.iter().for_each(|e| {
            e.iter().enumerate().for_each(|(cid, warp)| {
                warp.iter().enumerate().for_each(|(wid, mask)| {
                    stalls[cid][wid] |= mask;
                });
            });
        });
    }
}