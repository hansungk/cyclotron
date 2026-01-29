use serde::Serialize;

use crate::timeflow::{GmemStats, SmemStats};

#[derive(Debug, Clone, Default)]
pub struct CoreStats {
    pub gmem: GmemStats,
    pub smem: SmemStats,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SchedulerSummary {
    pub cycles: u64,
    pub active_warps_sum: u64,
    pub eligible_warps_sum: u64,
    pub issued_warps_sum: u64,
    pub issue_width: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SmemUtilSummary {
    pub cycles: u64,
    pub lane_busy_sum: u64,
    pub lane_total: u64,
    pub bank_busy_sum: u64,
    pub bank_total: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SmemConflictSummary {
    pub instructions: u64,
    pub active_lanes: u64,
    pub conflict_lanes: u64,
    pub unique_banks: u64,
    pub unique_subbanks: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct GmemHitSummary {
    pub l0_accesses: u64,
    pub l0_hits: u64,
    pub l1_accesses: u64,
    pub l1_hits: u64,
    pub l2_accesses: u64,
    pub l2_hits: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LatencySummary {
    pub gmem_count: u64,
    pub gmem_sum: u64,
    pub smem_count: u64,
    pub smem_sum: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorePerfSummary {
    pub core_id: usize,
    pub cluster_id: usize,
    pub scheduler: SchedulerSummary,
    pub smem_util: SmemUtilSummary,
    pub smem_conflicts: SmemConflictSummary,
    pub gmem_hits: GmemHitSummary,
    pub latencies: LatencySummary,
    pub gmem_stats: GmemStats,
    pub smem_stats: SmemStats,
    pub dma_completed: u64,
    pub tensor_completed: u64,
}
