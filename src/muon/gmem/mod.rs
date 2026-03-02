use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::sim::log::Logger;
use crate::sim::perf_log::PerfLogSession;
use crate::timeflow::{
    CoreGraph, FenceRequest, GmemPolicyConfig, GmemRequest, SmemFlowConfig, SmemRequest,
    WarpIssueScheduler, WritebackPayload,
};
use crate::timeq::Cycle;

mod completions;
mod core;
mod issue;
mod metrics;
mod pending;
mod split;

#[cfg(test)]
mod tests;

pub use metrics::*;

pub struct CoreTimingModel {
    graph: CoreGraph,
    pending_writeback: VecDeque<WritebackPayload>,
    pending_dma: VecDeque<u32>,
    pending_tensor: VecDeque<u32>,
    issue_scheduler: WarpIssueScheduler,
    pending_fence: VecDeque<FenceRequest>,
    fence_inflight: Vec<Option<u64>>,
    icache_inflight: Vec<Option<IcacheInflight>>,
    pending_cluster_gmem: VecDeque<PendingClusterIssue<GmemRequest>>,
    pending_cluster_smem: VecDeque<PendingClusterIssue<SmemRequest>>,
    pending_gmem: Vec<VecDeque<(u64, Cycle)>>,
    pending_smem: Vec<VecDeque<(u64, Cycle)>>,
    pending_execute: Vec<Option<Cycle>>,
    gmem_issue_cycle: HashMap<u64, Cycle>,
    smem_issue_cycle: HashMap<u64, Cycle>,
    core_id: usize,
    cluster_id: usize,
    gmem_policy: GmemPolicyConfig,
    smem_config: SmemFlowConfig,
    gmem_stats_range: Option<crate::timeflow::gmem::GmemStatsRange>,
    next_gmem_id: u64,
    next_smem_id: u64,
    next_icache_id: u64,
    logger: Arc<Logger>,
    perf_log_session: Option<Arc<PerfLogSession>>,
    log_stats: bool,
    stats_log_period: Cycle,
    last_stats_log_cycle: Option<Cycle>,
    last_logged_gmem_completed: u64,
    last_logged_smem_completed: u64,
    last_metrics_cycle: Option<Cycle>,
    last_issue_stats_cycle: Option<Cycle>,
    scheduler_stats: SchedulerSummary,
    smem_util: SmemUtilSummary,
    execute_util: ExecuteUtilSummary,
    smem_conflicts_summary: SmemConflictSummary,
    gmem_hits: GmemHitSummary,
    latencies: LatencySummary,
    dma_util: BasicUtilSummary,
    tensor_util: BasicUtilSummary,
    gmem_latency_hist: LatencyHistogram,
    smem_latency_hist: LatencyHistogram,
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
