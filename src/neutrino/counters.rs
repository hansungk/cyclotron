use std::cmp::Ordering;
use std::sync::Arc;
use crate::base::module::IsModule;
use crate::base::behavior::{ModuleBehaviors, Parameterizable};
use crate::base::module::{module, ModuleBase};
use crate::neutrino::config::NeutrinoConfig;
use crate::neutrino::scoreboard::{JobID, JobStatus};

pub fn counter_cmp(a: u32, b: u32, n: usize) -> Ordering {
    let max = 1u64 << n;
    let forward = |x: u64, y: u64| -> u64 {
        (y.wrapping_sub(x)) & (max - 1u64)
    };

    let dist_ab = forward(a as u64, b as u64);
    let dist_ba = forward(b as u64, a as u64);

    dist_ab.cmp(&dist_ba)
}

#[derive(Default)]
pub struct CountersState {
    counters: Vec<u32>,
}

pub struct Counters {
    base: ModuleBase<CountersState, NeutrinoConfig>,
}

impl Counters {
    pub fn new(config: Arc<NeutrinoConfig>) -> Self {
        let mut me = Counters {
            base: ModuleBase::<CountersState, NeutrinoConfig> {
                state: CountersState {
                    counters: vec![0; config.num_entries],
                },
                ..ModuleBase::default()
            }
        };
        me.init_conf(config);
        me
    }

    pub fn succ(&self, counter: u32) -> u32 {
        counter.wrapping_add(1) & ((1u32 << self.conf().counter_width) - 1)
    }

    pub fn pred(&self, counter: u32) -> u32 {
        counter.wrapping_sub(1) & ((1u32 << self.conf().counter_width) - 1)
    }

    pub fn inc(&mut self, task: u32) {
        assert!(task < self.conf().num_entries as u32, "task id out of range");
        let counter_width = self.conf().counter_width;
        let ctr = &mut self.base.state.counters[task as usize];
        *ctr = (ctr.wrapping_add(1)) & ((1u32 << counter_width) - 1);
    }

    pub fn peek(&self, task: u32) -> JobID {
        assert!(task < self.conf().num_entries as u32, "task id out of range");
        JobID {
            task_id: task,
            counter:  self.base.state.counters[task as usize]
        }
    }

    pub fn take(&mut self, task: u32) -> JobID {
        self.inc(task);
        self.peek(task)
    }

    pub fn check(&self, job: JobID) -> JobStatus {
        match counter_cmp(job.counter, self.peek(job.task_id).counter, self.conf().counter_width) {
            Ordering::Less => JobStatus::Finished,
            Ordering::Equal => JobStatus::RunningOrFinished, // if not in scoreboard, it's finished
            Ordering::Greater => JobStatus::NotStarted,
        }
    }
}

module!(Counters, CountersState, NeutrinoConfig,);

impl ModuleBehaviors for Counters {
    fn tick_one(&mut self) {}

    fn reset(&mut self) {
        self.base.state.counters = vec![0; self.conf().num_entries];
    }
}
