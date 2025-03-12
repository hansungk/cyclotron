use crate::base::behavior::ModuleBehaviors;
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
    base: ModuleBase<CommandProcessorState, MuonConfig>,
}

const REGFILE_USAGE_PER_BLOCK: isize = 32;
const SMEM_USAGE_PER_BLOCK: isize = 32;
const REGFILE_SIZE_PER_CLUSTER: isize = 64;
const SMEM_SIZE_PER_CLUSTER: isize = 64;

#[derive(Default)]
struct CommandProcessorState {
    remaining_threadblocks: isize,
    clusters: Vec<ClusterScheduleState>,
}

#[derive(Clone)]
struct ClusterScheduleState {
    running_threadblocks: isize,
    regfile_usage: isize,
    smem_usage: isize,
}

impl CommandProcessorState {
    pub fn new(num_clusters: usize, num_threadblocks: usize) -> Self {
        let clusters = vec![
            ClusterScheduleState {
                running_threadblocks: 0,
                regfile_usage: 0,
                smem_usage: 0,
            };
            num_clusters
        ];
        CommandProcessorState {
            remaining_threadblocks: num_threadblocks as isize,
            clusters,
        }
    }
}

impl CommandProcessor {
    pub fn new(_config: Arc<MuonConfig>, num_threadblocks: usize) -> Self {
        CommandProcessor {
            base: ModuleBase::<CommandProcessorState, MuonConfig> {
                state: CommandProcessorState::new(1 /*FIXME: num_clusters*/, num_threadblocks),
                ..ModuleBase::default()
            },
        }
    }

    pub fn schedule(&mut self) -> Vec<bool> {
        if self.base.state.remaining_threadblocks <= 0 {
            assert!(self.base.state.remaining_threadblocks == 0);
            return vec![false];
        }
        let schedule_per_cluster = |cluster: &mut ClusterScheduleState| -> bool {
            let new_regfile = cluster.regfile_usage + REGFILE_USAGE_PER_BLOCK;
            let new_smem = cluster.smem_usage + SMEM_USAGE_PER_BLOCK;
            if new_regfile <= REGFILE_SIZE_PER_CLUSTER && new_smem <= SMEM_SIZE_PER_CLUSTER {
                // TODO: schedule more than 1 block at a time
                cluster.running_threadblocks += 1;
                cluster.regfile_usage = new_regfile;
                cluster.smem_usage = new_smem;
                true
            } else {
                false
            }
        };
        let schedule: Vec<bool> = self
            .base
            .state
            .clusters
            .iter_mut()
            .map(schedule_per_cluster)
            .collect();
        let schedule_count: isize = schedule.iter().map(|&b| b as isize).sum();
        self.base.state.remaining_threadblocks -= schedule_count;
        schedule
    }

    pub fn retire(&mut self, cluster_id: usize, retired_threadblock: usize) {
        let cluster = &mut self.base.state.clusters[cluster_id];
        cluster.running_threadblocks -= retired_threadblock as isize;
        cluster.regfile_usage -= REGFILE_USAGE_PER_BLOCK * retired_threadblock as isize;
        cluster.smem_usage -= SMEM_USAGE_PER_BLOCK * retired_threadblock as isize;
        assert!(cluster.running_threadblocks >= 0);
        assert!(cluster.regfile_usage >= 0);
        assert!(cluster.smem_usage >= 0);
    }
}

impl ModuleBehaviors for CommandProcessor {
    fn tick_one(&mut self) {}
}
