use serde::Serialize;
use std::ops::AddAssign;

use crate::timeflow::{
    BarrierSummary, GmemStats, IcacheStats, LsuStats, SmemStats, WritebackStats,
};

#[derive(Debug, Clone, Default)]
pub struct CoreStats {
    pub gmem: GmemStats,
    pub smem: SmemStats,
    pub icache: IcacheStats,
    pub lsu: LsuStats,
    pub writeback: WritebackStats,
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

impl AddAssign<&SchedulerSummary> for SchedulerSummary {
    fn add_assign(&mut self, other: &SchedulerSummary) {
        self.cycles = self.cycles.saturating_add(other.cycles);
        self.active_warps_sum = self.active_warps_sum.saturating_add(other.active_warps_sum);
        self.eligible_warps_sum = self
            .eligible_warps_sum
            .saturating_add(other.eligible_warps_sum);
        self.issued_warps_sum = self.issued_warps_sum.saturating_add(other.issued_warps_sum);
        self.issue_width = self.issue_width.max(other.issue_width);
    }
}

impl AddAssign<&SmemUtilSummary> for SmemUtilSummary {
    fn add_assign(&mut self, other: &SmemUtilSummary) {
        self.cycles = self.cycles.saturating_add(other.cycles);
        self.lane_busy_sum = self.lane_busy_sum.saturating_add(other.lane_busy_sum);
        self.lane_total = self.lane_total.max(other.lane_total);
        self.bank_busy_sum = self.bank_busy_sum.saturating_add(other.bank_busy_sum);
        self.bank_total = self.bank_total.max(other.bank_total);
    }
}

impl AddAssign<&ExecuteUtilSummary> for ExecuteUtilSummary {
    fn add_assign(&mut self, other: &ExecuteUtilSummary) {
        self.cycles = self.cycles.saturating_add(other.cycles);
        self.int_busy_sum = self.int_busy_sum.saturating_add(other.int_busy_sum);
        self.int_mul_busy_sum = self.int_mul_busy_sum.saturating_add(other.int_mul_busy_sum);
        self.int_div_busy_sum = self.int_div_busy_sum.saturating_add(other.int_div_busy_sum);
        self.fp_busy_sum = self.fp_busy_sum.saturating_add(other.fp_busy_sum);
        self.sfu_busy_sum = self.sfu_busy_sum.saturating_add(other.sfu_busy_sum);
    }
}

impl AddAssign<&BasicUtilSummary> for BasicUtilSummary {
    fn add_assign(&mut self, other: &BasicUtilSummary) {
        self.cycles = self.cycles.saturating_add(other.cycles);
        self.busy_sum = self.busy_sum.saturating_add(other.busy_sum);
    }
}

impl AddAssign<&StallSummary> for StallSummary {
    fn add_assign(&mut self, other: &StallSummary) {
        self.gmem_queue_full = self.gmem_queue_full.saturating_add(other.gmem_queue_full);
        self.gmem_busy = self.gmem_busy.saturating_add(other.gmem_busy);
        self.smem_queue_full = self.smem_queue_full.saturating_add(other.smem_queue_full);
        self.smem_busy = self.smem_busy.saturating_add(other.smem_busy);
    }
}

impl AddAssign<&SmemConflictSummary> for SmemConflictSummary {
    fn add_assign(&mut self, other: &SmemConflictSummary) {
        self.instructions = self.instructions.saturating_add(other.instructions);
        self.active_lanes = self.active_lanes.saturating_add(other.active_lanes);
        self.conflict_lanes = self.conflict_lanes.saturating_add(other.conflict_lanes);
        self.unique_banks = self.unique_banks.saturating_add(other.unique_banks);
        self.unique_subbanks = self.unique_subbanks.saturating_add(other.unique_subbanks);
    }
}

impl AddAssign<&GmemHitSummary> for GmemHitSummary {
    fn add_assign(&mut self, other: &GmemHitSummary) {
        self.l0_accesses = self.l0_accesses.saturating_add(other.l0_accesses);
        self.l0_hits = self.l0_hits.saturating_add(other.l0_hits);
        self.l1_accesses = self.l1_accesses.saturating_add(other.l1_accesses);
        self.l1_hits = self.l1_hits.saturating_add(other.l1_hits);
        self.l2_accesses = self.l2_accesses.saturating_add(other.l2_accesses);
        self.l2_hits = self.l2_hits.saturating_add(other.l2_hits);
    }
}

impl AddAssign<&LatencySummary> for LatencySummary {
    fn add_assign(&mut self, other: &LatencySummary) {
        self.gmem_count = self.gmem_count.saturating_add(other.gmem_count);
        self.gmem_sum = self.gmem_sum.saturating_add(other.gmem_sum);
        self.smem_count = self.smem_count.saturating_add(other.smem_count);
        self.smem_sum = self.smem_sum.saturating_add(other.smem_sum);
    }
}

impl AddAssign<&LatencyHistogram> for LatencyHistogram {
    fn add_assign(&mut self, other: &LatencyHistogram) {
        self.accumulate(other);
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
    pub icache_stats: IcacheStats,
    pub lsu_stats: LsuStats,
    pub writeback_stats: WritebackStats,
    pub barrier_summary: BarrierSummary,
    pub dma_completed: u64,
    pub tensor_completed: u64,
    pub stall_summary: StallSummary,
    pub gmem_latency_hist: LatencyHistogram,
    pub smem_latency_hist: LatencyHistogram,
}
