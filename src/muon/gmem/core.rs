use std::collections::VecDeque;
use std::env;
use std::sync::Arc;

use crate::info;
use crate::muon::scheduler::Scheduler;
use crate::sim::log::Logger;
use crate::sim::perf_log;
use crate::timeflow::{
    BarrierManager, ClusterGmemGraph, CoreGraph, CoreGraphConfig, DmaQueue, ExecutePipeline,
    FenceQueue, IcacheSubgraph, LsuSubgraph, OperandFetchQueue, TensorQueue, WarpIssueScheduler,
    WritebackQueue,
};
use crate::timeq::Cycle;

use super::{CorePerfSummary, CoreStats, CoreTimingModel, GmemLevelSummary, StallSummary};

impl CoreTimingModel {
    pub fn new(
        config: CoreGraphConfig,
        num_warps: usize,
        core_id: usize,
        cluster_id: usize,
        cluster_gmem: Arc<std::sync::RwLock<ClusterGmemGraph>>,
        logger: Arc<Logger>,
    ) -> Self {
        let log_stats = env::var("CYCLOTRON_TIMING_LOG_STATS")
            .ok()
            .map(|val| {
                let lowered = val.to_ascii_lowercase();
                lowered == "1" || lowered == "true" || lowered == "yes"
            })
            .unwrap_or(false);
        Self::with_options(
            config,
            num_warps,
            core_id,
            cluster_id,
            cluster_gmem,
            logger,
            log_stats,
        )
    }

    pub fn with_options(
        config: CoreGraphConfig,
        num_warps: usize,
        core_id: usize,
        cluster_id: usize,
        cluster_gmem: Arc<std::sync::RwLock<ClusterGmemGraph>>,
        logger: Arc<Logger>,
        log_stats: bool,
    ) -> Self {
        let lsu = LsuSubgraph::new(config.memory.lsu.clone(), num_warps);
        let gmem_policy = config.memory.gmem.policy;
        let smem_config = config.memory.smem.clone();
        let icache = IcacheSubgraph::new(config.memory.icache.clone());
        let operand_fetch = OperandFetchQueue::new(
            config.memory.operand_fetch.enabled,
            config.memory.operand_fetch.queue,
        );
        let writeback = WritebackQueue::new(config.memory.writeback.clone());
        let barrier = BarrierManager::new(config.io.barrier.clone(), num_warps);
        let fence = FenceQueue::new(config.io.fence.clone());
        let dma = DmaQueue::new(config.io.dma.clone());
        let tensor = TensorQueue::new(config.compute.tensor.clone());
        let execute_pipeline = ExecutePipeline::new(config.compute.execute.clone());
        let issue_scheduler = WarpIssueScheduler::new(config.compute.scheduler.clone());
        let mut scheduler_stats = super::SchedulerSummary::default();
        scheduler_stats.issue_width = config.compute.scheduler.issue_width.max(1) as u64;
        let stats_log_period = env::var("CYCLOTRON_STATS_LOG_PERIOD")
            .ok()
            .and_then(|val| val.parse::<Cycle>().ok())
            .unwrap_or(100)
            .max(1);

        Self {
            graph: CoreGraph::new(config),
            lsu,
            icache,
            operand_fetch,
            writeback,
            pending_writeback: VecDeque::new(),
            pending_dma: VecDeque::new(),
            pending_tensor: VecDeque::new(),
            issue_scheduler,
            barrier,
            barrier_inflight: vec![false; num_warps],
            fence,
            pending_fence: VecDeque::new(),
            fence_inflight: vec![None; num_warps],
            dma,
            tensor,
            execute_pipeline,
            icache_inflight: vec![None; num_warps],
            pending_cluster_gmem: VecDeque::new(),
            pending_cluster_smem: VecDeque::new(),
            pending_gmem: vec![VecDeque::new(); num_warps],
            pending_smem: vec![VecDeque::new(); num_warps],
            pending_execute: vec![None; num_warps],
            gmem_issue_cycle: std::collections::HashMap::new(),
            smem_issue_cycle: std::collections::HashMap::new(),
            cluster_gmem,
            core_id,
            cluster_id,
            gmem_policy,
            smem_config,
            next_gmem_id: 0,
            next_smem_id: 0,
            next_icache_id: 0,
            logger,
            log_stats,
            stats_log_period,
            last_stats_log_cycle: None,
            last_logged_gmem_completed: 0,
            last_logged_smem_completed: 0,
            last_metrics_cycle: None,
            last_issue_stats_cycle: None,
            scheduler_stats,
            smem_util: super::SmemUtilSummary::default(),
            execute_util: super::ExecuteUtilSummary::default(),
            smem_conflicts_summary: super::SmemConflictSummary::default(),
            gmem_hits: super::GmemHitSummary::default(),
            latencies: super::LatencySummary::default(),
            dma_util: super::BasicUtilSummary::default(),
            tensor_util: super::BasicUtilSummary::default(),
            gmem_latency_hist: super::LatencyHistogram::default(),
            smem_latency_hist: super::LatencyHistogram::default(),
        }
    }

    pub fn tick(&mut self, now: Cycle, scheduler: &mut Scheduler) {
        self.icache.tick(now);
        self.lsu.tick(now);
        self.operand_fetch.tick(now, |_| {});
        self.dma.tick(now);
        self.tensor.tick(now);
        self.execute_pipeline.tick(now);
        if let Some(released) = self.barrier.tick(now) {
            for warp in released {
                if let Some(slot) = self.barrier_inflight.get_mut(warp) {
                    *slot = false;
                }
                scheduler.clear_resource_wait(warp);
            }
        }
        self.drive_lsu_issues(now);
        self.issue_pending_cluster_gmem(now);
        self.issue_pending_cluster_smem(now);

        let prev_gmem = self
            .cluster_gmem
            .read()
            .unwrap()
            .stats(self.core_id)
            .completed();
        let prev_smem = self.graph.smem_stats().completed;
        let mut gmem_completions = Vec::new();
        let mut gmem_stats_snapshot = None;
        {
            let mut cluster = self.cluster_gmem.write().unwrap();
            cluster.tick(now);

            while let Some(completion) = cluster.pop_completion(self.core_id) {
                gmem_completions.push(completion);
            }

            if self.log_stats {
                gmem_stats_snapshot = Some(cluster.stats(self.core_id));
            }
        }

        self.graph.tick(now);
        let mut smem_completions = Vec::new();
        while let Some(completion) = self.graph.pop_smem_completion() {
            smem_completions.push(completion);
        }

        self.drain_pending_writeback(now);
        self.drain_pending_fence(now);
        self.drain_pending_dma(now);
        self.drain_pending_tensor(now);

        for completion in gmem_completions {
            let is_flush =
                completion.request.kind.is_flush_l0() || completion.request.kind.is_flush_l1();
            if is_flush {
                self.handle_gmem_completion(now, completion.clone(), scheduler);
                self.enqueue_fence(
                    now,
                    crate::timeflow::FenceRequest {
                        warp: completion.request.warp,
                        request_id: completion.request.id,
                    },
                );
                continue;
            }
            self.enqueue_writeback(now, crate::timeflow::WritebackPayload::Gmem(completion));
        }

        for completion in smem_completions {
            self.enqueue_writeback(now, crate::timeflow::WritebackPayload::Smem(completion));
        }

        self.writeback.tick(now);
        self.fence.tick(now);

        while let Some(payload) = self.writeback.pop_ready() {
            match payload {
                crate::timeflow::WritebackPayload::Gmem(completion) => {
                    self.handle_gmem_completion(now, completion, scheduler);
                }
                crate::timeflow::WritebackPayload::Smem(completion) => {
                    self.handle_smem_completion(now, completion, scheduler);
                }
            }
        }

        while let Some(fence_req) = self.fence.pop_ready() {
            if let Some(slot) = self.fence_inflight.get_mut(fence_req.warp) {
                if slot.map(|id| id == fence_req.request_id).unwrap_or(false) {
                    *slot = None;
                }
            }
            scheduler.clear_resource_wait(fence_req.warp);
        }

        self.sample_metrics(now);

        if self.log_stats {
            if let Some(gmem_stats) = gmem_stats_snapshot {
                if gmem_stats.completed() != prev_gmem {
                    info!(
                        self.logger,
                        "[gmem] stats issued={} completed={} inflight={} queue_full_rejects={} busy_rejects={} bytes_issued={} bytes_completed={}",
                        gmem_stats.issued(),
                        gmem_stats.completed(),
                        gmem_stats.inflight(),
                        gmem_stats.queue_full_rejects(),
                        gmem_stats.busy_rejects(),
                        gmem_stats.bytes_issued(),
                        gmem_stats.bytes_completed()
                    );
                    self.last_logged_gmem_completed = gmem_stats.completed();
                }
            }

            let smem_stats = self.graph.smem_stats();
            if smem_stats.completed != prev_smem {
                info!(
                    self.logger,
                    "[smem] stats issued={} completed={} inflight={} queue_full_rejects={} busy_rejects={} bytes_issued={} bytes_completed={}",
                    smem_stats.issued,
                    smem_stats.completed,
                    smem_stats.inflight,
                    smem_stats.queue_full_rejects,
                    smem_stats.busy_rejects,
                    smem_stats.bytes_issued,
                    smem_stats.bytes_completed
                );
                self.last_logged_smem_completed = smem_stats.completed;
            }
        }
    }

    pub fn has_pending_gmem(&self, warp: usize) -> bool {
        self.pending_gmem
            .get(warp)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false)
    }

    pub fn outstanding_gmem(&self) -> usize {
        self.pending_gmem.iter().map(|queue| queue.len()).sum()
    }

    pub fn has_pending_smem(&self, warp: usize) -> bool {
        self.pending_smem
            .get(warp)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false)
    }

    pub fn outstanding_smem(&self) -> usize {
        self.pending_smem.iter().map(|queue| queue.len()).sum()
    }

    pub fn stats(&self) -> CoreStats {
        let gmem_stats = self.cluster_gmem.read().unwrap().stats(self.core_id);
        CoreStats {
            gmem: gmem_stats,
            smem: self.graph.smem_stats(),
        }
    }

    pub fn perf_summary(&self) -> CorePerfSummary {
        let stats = self.stats();
        let gmem_stats = stats.gmem;
        let smem_stats_snapshot = stats.smem.clone();
        let (l0_stats, l1_stats, l2_stats) = self
            .cluster_gmem
            .read()
            .unwrap()
            .hierarchy_stats_per_level();
        let gmem_level_stats = GmemLevelSummary {
            l0: l0_stats,
            l1: l1_stats,
            l2: l2_stats,
        };
        CorePerfSummary {
            core_id: self.core_id,
            cluster_id: self.cluster_id,
            scheduler: self.scheduler_stats,
            smem_util: self.smem_util,
            execute_util: self.execute_util,
            dma_bytes_issued: self.dma.bytes_issued(),
            dma_bytes_completed: self.dma.bytes_completed(),
            dma_util: self.dma_util,
            tensor_bytes_issued: self.tensor.bytes_issued(),
            tensor_bytes_completed: self.tensor.bytes_completed(),
            tensor_util: self.tensor_util,
            smem_conflicts: self.smem_conflicts_summary,
            gmem_hits: self.gmem_hits,
            latencies: self.latencies,
            gmem_stats,
            gmem_level_stats,
            smem_stats: smem_stats_snapshot.clone(),
            dma_completed: self.dma.completed(),
            tensor_completed: self.tensor.completed(),
            stall_summary: StallSummary {
                gmem_queue_full: gmem_stats.queue_full_rejects(),
                gmem_busy: gmem_stats.busy_rejects(),
                smem_queue_full: smem_stats_snapshot.queue_full_rejects,
                smem_busy: smem_stats_snapshot.busy_rejects,
            },
            gmem_latency_hist: self.gmem_latency_hist,
            smem_latency_hist: self.smem_latency_hist,
        }
    }

    pub fn clear_stats(&mut self) {
        self.cluster_gmem.write().unwrap().clear_stats(self.core_id);
        self.graph.clear_smem_stats();
        self.last_logged_gmem_completed = 0;
        self.last_logged_smem_completed = 0;
        self.execute_util = super::ExecuteUtilSummary::default();
        self.dma_util = super::BasicUtilSummary::default();
        self.tensor_util = super::BasicUtilSummary::default();
        self.gmem_latency_hist = super::LatencyHistogram::default();
        self.smem_latency_hist = super::LatencyHistogram::default();
        self.pending_execute
            .iter_mut()
            .for_each(|slot| *slot = None);
    }

    fn sample_metrics(&mut self, now: Cycle) {
        if self.last_metrics_cycle == Some(now) {
            return;
        }
        self.last_metrics_cycle = Some(now);
        self.smem_util.cycles = self.smem_util.cycles.saturating_add(1);
        self.execute_util.cycles = self.execute_util.cycles.saturating_add(1);
        self.dma_util.cycles = self.dma_util.cycles.saturating_add(1);
        if self.dma.is_busy() {
            self.dma_util.busy_sum = self.dma_util.busy_sum.saturating_add(1);
        }
        self.tensor_util.cycles = self.tensor_util.cycles.saturating_add(1);
        if self.tensor.is_busy() {
            self.tensor_util.busy_sum = self.tensor_util.busy_sum.saturating_add(1);
        }

        // Execute pipeline utilization is sampled as "busy this cycle" based on outstanding
        // requests in each TimedServer.
        use crate::timeflow::ExecUnitKind;
        if self.execute_pipeline.is_busy(ExecUnitKind::Int) {
            self.execute_util.int_busy_sum = self.execute_util.int_busy_sum.saturating_add(1);
        }
        if self.execute_pipeline.is_busy(ExecUnitKind::IntMul) {
            self.execute_util.int_mul_busy_sum =
                self.execute_util.int_mul_busy_sum.saturating_add(1);
        }
        if self.execute_pipeline.is_busy(ExecUnitKind::IntDiv) {
            self.execute_util.int_div_busy_sum =
                self.execute_util.int_div_busy_sum.saturating_add(1);
        }
        if self.execute_pipeline.is_busy(ExecUnitKind::Fp) {
            self.execute_util.fp_busy_sum = self.execute_util.fp_busy_sum.saturating_add(1);
        }
        if self.execute_pipeline.is_busy(ExecUnitKind::Sfu) {
            self.execute_util.sfu_busy_sum = self.execute_util.sfu_busy_sum.saturating_add(1);
        }
        // sample instantaneous utilization counters
        let sample = self.graph.sample_smem_utilization();
        self.smem_util.lane_busy_sum = self
            .smem_util
            .lane_busy_sum
            .saturating_add(sample.lane_busy as u64);
        self.smem_util.bank_busy_sum = self
            .smem_util
            .bank_busy_sum
            .saturating_add(sample.bank_busy as u64);
        self.smem_util.lane_total = sample.lane_total as u64;
        self.smem_util.bank_total = sample.bank_total as u64;

        // record per-bank busy samples and conflict attempts accumulated inside the
        // timeflow SMEM subgraph (bank_attempts / bank_conflicts maintained there).
        self.graph.record_smem_sample();

        // Periodically log aggregated SMEM stats so users can observe utilization
        // and bank conflict rate without opening CSVs. Use configured period.
        let period = self.smem_config.smem_log_period.max(1);
        if self.log_stats && now % period == 0 {
            let cycles = self.smem_util.cycles.max(1) as f64;
            let lane_total = self.smem_util.lane_total.max(1) as f64;
            let bank_total = self.smem_util.bank_total.max(1) as f64;
            let lane_busy = self.smem_util.lane_busy_sum as f64;
            let bank_busy = self.smem_util.bank_busy_sum as f64;
            let lane_util_pct = 100.0 * lane_busy / (cycles * lane_total);
            let bank_util_pct = 100.0 * bank_busy / (cycles * bank_total);

            let smem_stats = self.graph.smem_stats();
            let mut total_attempts: u64 = 0;
            let mut total_conflicts: u64 = 0;
            for a in &smem_stats.bank_attempts {
                total_attempts = total_attempts.saturating_add(*a);
            }
            for c in &smem_stats.bank_conflicts {
                total_conflicts = total_conflicts.saturating_add(*c);
            }
            let conflict_rate_pct = if total_attempts == 0 {
                0.0
            } else {
                100.0 * (total_conflicts as f64) / (total_attempts as f64)
            };

            info!(
                self.logger,
                "[smem] util lane={:.2}% bank={:.2}% conflicts={:.2}% attempts={} conflicts={}",
                lane_util_pct,
                bank_util_pct,
                conflict_rate_pct,
                total_attempts,
                total_conflicts
            );
        }
        self.emit_stats(now);
    }

    fn emit_stats(&mut self, now: Cycle) {
        if !self.log_stats || self.stats_log_period == 0 {
            return;
        }
        if now % self.stats_log_period != 0 {
            return;
        }
        if self.last_stats_log_cycle == Some(now) {
            return;
        }
        self.last_stats_log_cycle = Some(now);
        if let Some(logger) = perf_log::stats_logger() {
            let record = perf_log::StatsRecord {
                cycle: now,
                summary: self.perf_summary(),
            };
            logger.write(&record);
        }
    }

    pub fn dma_completed(&self) -> u64 {
        self.dma.completed()
    }

    pub fn tensor_completed(&self) -> u64 {
        self.tensor.completed()
    }

    pub(super) fn trace_event(
        &mut self,
        cycle: Cycle,
        event: &str,
        warp: usize,
        request_id: Option<u64>,
        bytes: u32,
        reason: Option<&str>,
    ) {
        let _ = (cycle, event, warp, request_id, bytes, reason);
    }
}
