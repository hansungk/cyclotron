use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::sim::log::Logger;
use crate::timeflow::{
    BarrierManager, ClusterGmemGraph, CoreGraph, DmaQueue, FenceQueue, FenceRequest,
    GmemPolicyConfig, GmemRequest, IcacheSubgraph, LsuSubgraph, OperandFetchQueue, SmemFlowConfig,
    SmemRequest, TensorQueue, WarpIssueScheduler, WritebackPayload, WritebackQueue,
};
use crate::timeq::Cycle;

mod metrics;
mod logging;
mod core;
mod issue;
mod split;
mod pending;
mod completions;

#[cfg(test)]
mod tests;

pub use metrics::*;

use logging::{LatencySink, SchedulerSink, SmemConflictSink, TraceSink};

pub struct CoreTimingModel {
    graph: CoreGraph,
    lsu: LsuSubgraph,
    icache: IcacheSubgraph,
    operand_fetch: OperandFetchQueue,
    writeback: WritebackQueue,
    pending_writeback: VecDeque<WritebackPayload>,
    pending_dma: VecDeque<u32>,
    pending_tensor: VecDeque<u32>,
    issue_scheduler: WarpIssueScheduler,
    barrier: BarrierManager,
    barrier_inflight: Vec<bool>,
    fence: FenceQueue,
    pending_fence: VecDeque<FenceRequest>,
    fence_inflight: Vec<Option<u64>>,
    dma: DmaQueue,
    tensor: TensorQueue,
    icache_inflight: Vec<Option<IcacheInflight>>,
    pending_cluster_gmem: VecDeque<PendingClusterIssue<GmemRequest>>,
    pending_cluster_smem: VecDeque<PendingClusterIssue<SmemRequest>>,
    pending_gmem: Vec<VecDeque<(u64, Cycle)>>,
    pending_smem: Vec<VecDeque<(u64, Cycle)>>,
    gmem_issue_cycle: HashMap<u64, Cycle>,
    smem_issue_cycle: HashMap<u64, Cycle>,
    cluster_gmem: Arc<std::sync::RwLock<ClusterGmemGraph>>,
    core_id: usize,
    cluster_id: usize,
    gmem_policy: GmemPolicyConfig,
    smem_config: SmemFlowConfig,
    next_gmem_id: u64,
    next_smem_id: u64,
    next_icache_id: u64,
    logger: Arc<Logger>,
    trace: Option<TraceSink>,
    mem_latency: Option<LatencySink>,
    smem_conflicts: Option<SmemConflictSink>,
    scheduler_log: Option<SchedulerSink>,
    log_stats: bool,
    last_logged_gmem_completed: u64,
    last_logged_smem_completed: u64,
    last_metrics_cycle: Option<Cycle>,
    last_issue_stats_cycle: Option<Cycle>,
    scheduler_stats: SchedulerSummary,
    smem_util: SmemUtilSummary,
    smem_conflicts_summary: SmemConflictSummary,
    gmem_hits: GmemHitSummary,
    latencies: LatencySummary,
}

struct PendingClusterIssue<T> {
    request: T,
    retry_at: Cycle,
}

#[derive(Clone, Copy)]
struct IcacheInflight {
    ready_at: Cycle,
}

#[derive(Debug, Clone, Copy)]
struct SmemConflictSample {
    active_lanes: u32,
    unique_banks: u32,
    unique_subbanks: u32,
    conflict_lanes: u32,
}
