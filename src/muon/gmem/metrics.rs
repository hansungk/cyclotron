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
pub struct ExecuteUtilSummary {
    pub cycles: u64,
    pub int_busy_sum: u64,
    pub int_mul_busy_sum: u64,
    pub int_div_busy_sum: u64,
    pub fp_busy_sum: u64,
    pub sfu_busy_sum: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BasicUtilSummary {
    pub cycles: u64,
    pub busy_sum: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StallSummary {
    pub gmem_queue_full: u64,
    pub gmem_busy: u64,
    pub smem_queue_full: u64,
    pub smem_busy: u64,
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

#[derive(Debug, Clone, Serialize)]
pub struct GmemLevelSummary {
    pub l0: GmemStats,
    pub l1: GmemStats,
    pub l2: GmemStats,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LatencySummary {
    pub gmem_count: u64,
    pub gmem_sum: u64,
    pub smem_count: u64,
    pub smem_sum: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LatencyHistogram {
    pub buckets: [u64; 6],
}

impl LatencyHistogram {
    pub fn record(&mut self, latency: u64) {
        let idx = match latency {
            0..=3 => 0,
            4..=7 => 1,
            8..=15 => 2,
            16..=31 => 3,
            32..=63 => 4,
            _ => 5,
        };
        self.buckets[idx] = self.buckets[idx].saturating_add(1);
    }

    pub fn accumulate(&mut self, other: &LatencyHistogram) {
        for (dst, src) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *dst = dst.saturating_add(*src);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CorePerfSummary {
    pub core_id: usize,
    pub cluster_id: usize,
    pub scheduler: SchedulerSummary,
    pub smem_util: SmemUtilSummary,
    pub execute_util: ExecuteUtilSummary,
    pub dma_bytes_issued: u64,
    pub dma_bytes_completed: u64,
    pub dma_util: BasicUtilSummary,
    pub tensor_bytes_issued: u64,
    pub tensor_bytes_completed: u64,
    pub tensor_util: BasicUtilSummary,
    pub smem_conflicts: SmemConflictSummary,
    pub gmem_hits: GmemHitSummary,
    pub latencies: LatencySummary,
    pub gmem_stats: GmemStats,
    pub gmem_level_stats: GmemLevelSummary,
    pub smem_stats: SmemStats,
    pub dma_completed: u64,
    pub tensor_completed: u64,
    pub stall_summary: StallSummary,
    pub gmem_latency_hist: LatencyHistogram,
    pub smem_latency_hist: LatencyHistogram,
}
