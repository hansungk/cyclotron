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
        let num_clusters = self.base.state.clusters.len();

        if self.base.state.remaining_threadblocks <= 0 {
            assert!(self.base.state.remaining_threadblocks == 0);
            return vec![false; num_clusters];
        }
        
        let can_schedule_per_cluster = |cluster: &mut ClusterScheduleState| -> usize {
            let from_regfile_limit = (REGFILE_SIZE_PER_CLUSTER - cluster.regfile_usage) / REGFILE_USAGE_PER_BLOCK;
            let from_smem_limit = (SMEM_SIZE_PER_CLUSTER - cluster.smem_usage) / SMEM_USAGE_PER_BLOCK;
            std::cmp::min(from_regfile_limit, from_smem_limit) as usize
        };
        
        // TODO: schedule more than one threadblock per cluster if possible
        let mut schedule: Vec<bool> = Vec::with_capacity(self.base.state.clusters.len());
        for cluster in self.base.state.clusters.iter_mut() {
            if self.base.state.remaining_threadblocks > 0 && can_schedule_per_cluster(cluster) > 0 {
                cluster.running_threadblocks += 1;
                cluster.regfile_usage += REGFILE_USAGE_PER_BLOCK;
                cluster.smem_usage += SMEM_USAGE_PER_BLOCK;
                self.base.state.remaining_threadblocks -= 1;
                schedule.push(true);
            } else {
                schedule.push(false);
            }
        }

        schedule
    }

    pub fn retire(&mut self, cluster_id: usize, retired_threadblock: usize) {
        let cluster = &mut self.base.state.clusters[cluster_id];
        cluster.running_threadblocks -= retired_threadblock as isize;
        cluster.regfile_usage -= REGFILE_USAGE_PER_BLOCK * retired_threadblock as isize;
        cluster.smem_usage -= SMEM_USAGE_PER_BLOCK * retired_threadblock as isize;

        // Sanity checks
        assert!(cluster.running_threadblocks >= 0);
        assert!(cluster.regfile_usage >= 0);
        assert!(cluster.smem_usage >= 0);
    }
}

impl ModuleBehaviors for CommandProcessor {
    fn tick_one(&mut self) {}
}