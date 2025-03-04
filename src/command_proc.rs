use crate::base::module::ModuleBase;
use crate::muon::config::MuonConfig;
use std::sync::Arc;

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

#[derive(Default)]
struct CommandProcessorState {
    regfile_usage: usize,
    smem_usage: usize,
}

impl CommandProcessor {
    pub fn new() -> Self {
        CommandProcessor {
            base: ModuleBase::<CommandProcessorState, ()> {
                state: CommandProcessorState::default(),
                ..ModuleBase::default()
            },
        }
    }
}
