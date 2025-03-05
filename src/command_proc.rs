use crate::base::behavior::ModuleBehaviors;
use crate::base::module::ModuleBase;

/// The command processor schedules threadblocks of a kernel onto clusters (CUs/SMs).
///
/// Assumptions:
/// - Only one kernel scheduled at the device at a time
/// - Scheduling policy is round-robin across all available clusters, where
///   available clusters mean clusters that have free space in both the register file and the
///   shared memory that are larger than the static usage of a single threadblock
pub struct CommandProcessor {
    base: ModuleBase<CommandProcessorState, ()>,
}

const REGFILE_USAGE_PER_BLOCK: isize = 32;
const SMEM_USAGE_PER_BLOCK: isize = 32;
const REGFILE_SIZE_PER_CLUSTER: isize = 64;
const SMEM_SIZE_PER_CLUSTER: isize = 64;

#[derive(Default)]
struct CommandProcessorState {
    remaining_threadblocks: isize,
    // TODO: these should be per cluster
    running_threadblocks: isize,
    regfile_usage: isize,
    smem_usage: isize,
}

impl CommandProcessorState {
    pub fn new(num_threadblocks: usize) -> Self {
        CommandProcessorState {
            remaining_threadblocks: num_threadblocks as isize,
            running_threadblocks: 0,
            regfile_usage: 0,
            smem_usage: 0,
        }
    }
}

impl CommandProcessor {
    pub fn new(num_threadblocks: usize) -> Self {
        CommandProcessor {
            base: ModuleBase::<CommandProcessorState, ()> {
                state: CommandProcessorState::new(num_threadblocks),
                ..ModuleBase::default()
            },
        }
    }

    pub fn schedule(&mut self) -> bool {
        if self.base.state.remaining_threadblocks <= 0 {
            assert!(self.base.state.remaining_threadblocks == 0);
            return false;
        }
        let new_regfile = self.base.state.regfile_usage + REGFILE_USAGE_PER_BLOCK;
        let new_smem = self.base.state.smem_usage + SMEM_USAGE_PER_BLOCK;
        if new_regfile <= REGFILE_SIZE_PER_CLUSTER && new_smem <= SMEM_SIZE_PER_CLUSTER {
            // TODO: schedule more than 1 block at a time
            self.base.state.remaining_threadblocks -= 1;
            self.base.state.running_threadblocks += 1;
            self.base.state.regfile_usage = new_regfile;
            self.base.state.smem_usage = new_smem;
            true
        } else {
            false
        }
    }

    pub fn retire(&mut self, retired_threadblock: usize) {
        self.base.state.running_threadblocks -= retired_threadblock as isize;
        self.base.state.regfile_usage -= REGFILE_USAGE_PER_BLOCK * retired_threadblock as isize;
        self.base.state.smem_usage -= SMEM_USAGE_PER_BLOCK * retired_threadblock as isize;
        assert!(self.base.state.running_threadblocks >= 0);
        assert!(self.base.state.regfile_usage >= 0);
        assert!(self.base.state.smem_usage >= 0);
    }
}

impl ModuleBehaviors for CommandProcessor {
    fn tick_one(&mut self) {}
}
