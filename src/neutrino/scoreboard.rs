use std::cmp::PartialEq;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use log::info;
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
pub enum NeutrinoRetMode {
    Immediate,
    Hardware,
    Manual,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum NeutrinoPartMode {
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
    ret_mode: NeutrinoRetMode,
    part_mode: NeutrinoPartMode,
    sync: bool,
    num_elems: u32,
    arrived_mask: Vec<Vec<u32>>, // mask[wid] = thread mask
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
    pub cmd_type: NeutrinoCmdType,
    pub job_id: Option<JobID>, // for payload/complete
    pub task: u32,
    pub deps: Vec<JobID>,
    pub ret_mode: NeutrinoRetMode,
    pub part_mode: NeutrinoPartMode,
    pub num_elems: u32,
    pub sync: bool,
    pub tmask: u32,
}

impl Display for NeutrinoCmd {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "neutrino [type: {}, job_id: {:?}, task: {}, deps: {:?}, \
            ret_mode: {}, part_mode: {}, num_elems: {}, sync: {}, tmask: {:08x}]",
            match self.cmd_type {
                NeutrinoCmdType::Invoke => "invoke",
                NeutrinoCmdType::Payload => "payload",
                NeutrinoCmdType::Complete => "complete",
            },
            self.job_id,
            self.task,
            self.deps,
            match self.ret_mode {
                NeutrinoRetMode::Immediate => "immediate",
                NeutrinoRetMode::Hardware => "hardware",
                NeutrinoRetMode::Manual => "manual",
            },
            match self.part_mode {
                NeutrinoPartMode::Threads => "threads",
                NeutrinoPartMode::Warps => "warps",
                NeutrinoPartMode::ThreadBlocks => "thread blocks",
            },
            self.num_elems,
            self.sync,
            self.tmask
        )
    }
}

impl Display for JobID {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "job[{} | {}]", self.task_id, self.counter)
    }
}

pub struct Scoreboard {
    base: ModuleBase<ScoreboardState, NeutrinoConfig>,
    counters: Counters,
}

module!(Scoreboard, ScoreboardState, NeutrinoConfig,);

impl ModuleBehaviors for Scoreboard {
    fn tick_one(&mut self) {
        self.base.cycle += 1;
    }

    fn reset(&mut self) {
        self.counters.reset();
        self.base.state.entries.clear();
    }
}

impl Scoreboard {
    pub fn new(config: Arc<NeutrinoConfig>) -> Self {
        let counters = Counters::new(config.clone());
        let mut me = Scoreboard {
            base: ModuleBase::<ScoreboardState, NeutrinoConfig> {
                state: ScoreboardState {
                    entries: HashMap::new(),
                },
                ..ModuleBase::default()
            },
            counters,
        };
        me.init_conf(config);
        me
    }

    pub fn job_id_from_u32(&self, id: u32) -> JobID {
        let task_id = (id >> self.conf().counter_width) & ((1 << self.conf().task_id_width) - 1);
        let counter = id & ((1 << self.conf().counter_width) - 1);
        JobID { task_id, counter }
    }

    pub fn u32_from_job_id(&self, job_id: JobID) -> u32 {
        job_id.task_id << self.conf().counter_width | job_id.counter
    }

    pub fn arrive(&mut self, cmd: &NeutrinoCmd, cid: usize, wid: usize) -> JobID {
        let num_cores = self.conf().muon_config.num_cores;
        let num_warps = self.conf().muon_config.num_warps;
        assert!(cid < num_cores, "core id out of range");
        assert!(wid < num_warps, "warp id out of range");
        // assert!(cmd.task > 0, "task 0 is reserved");
        match cmd.cmd_type {
            NeutrinoCmdType::Invoke => {
                // first try to find existing non-dispatched entry
                let existing_job_id = self.counters.peek(cmd.task);
                let entry_opt = self.base.state.entries.get_mut(&existing_job_id);
                let use_existing = entry_opt.as_ref().is_some_and(|e| !e.dispatched);
                if use_existing {
                    // TODO: warn if existing parameters mismatch incoming
                    let entry = entry_opt.unwrap();
                    entry.arrived_mask[cid][wid] |= cmd.tmask;
                    existing_job_id
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
                        arrived_mask: vec![vec![0u32; num_warps]; num_cores],
                        dispatched: false,
                    };
                    entry.arrived_mask[cid][wid] = cmd.tmask;
                    self.base.state.entries.insert(job_id, entry);
                    job_id
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

    pub fn verify(&self, job_id: JobID) {
        assert!(self.base.state.entries.contains_key(&job_id),
                "job {} not found", job_id.task_id)
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
            x.iter().filter(|y| **y != 0u32)
        }).collect::<Vec<_>>().len() as u32;

        let participation_satisfied = (match entry.part_mode {
            NeutrinoPartMode::Threads => num_threads,
            NeutrinoPartMode::Warps => num_warps,
            NeutrinoPartMode::ThreadBlocks => todo!(),
        }) >= entry.num_elems;

        let dependencies_satisfied = entry.deps.iter().map(|dep| {
            (dep.task_id == 0) || // task 0 is reserved for no dependency
                (!self.base.state.entries.contains_key(dep) &&
                    (self.counters.check(*dep) != NotStarted))
        }).all(|x| x);

        // if either the previous job doesn't exist or is dispatched, then it's in order
        let order_satisfied = match self.base.state.entries.get(&JobID {
            task_id: job_id.task_id,
            counter: self.counters.pred(job_id.counter),
        }) {
            None => true,
            Some(e) => e.dispatched
        };

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

        if entry.ret_mode == NeutrinoRetMode::Immediate {
            self.complete(job_id)
        }
    }

    pub fn complete(&mut self, job_id: JobID) {
        assert!(self.is_ready(job_id));
        let entry = self.base.state.entries.get(&job_id).unwrap();
        assert!(entry.dispatched);
        self.base.state.entries.remove_entry(&job_id);
    }

    pub fn get_stalls(&mut self) -> Vec<Vec<u32>> {
        // find all sync, undispatched jobs and OR their masks
        let mut stalls = vec![
            vec![0u32; self.conf().muon_config.num_warps];
            self.conf().muon_config.num_cores];
        let arrived_masks: Vec<Vec<Vec<u32>>> = self.base.state.entries.iter()
            .filter_map(|e| { e.1.sync.then_some(e.1.arrived_mask.clone()) })
            .collect::<Vec<_>>();
        arrived_masks.iter().for_each(|e| {
            e.iter().enumerate().for_each(|(cid, warp)| {
                warp.iter().enumerate().for_each(|(wid, mask)| {
                    stalls[cid][wid] |= mask;
                });
            });
        });
        info!("stalls {:?}", stalls);
        stalls
    }
}
