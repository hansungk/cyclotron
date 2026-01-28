use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::info;
use crate::muon::scheduler::Scheduler;
use crate::sim::log::Logger;
use crate::timeflow::{
    ClusterGmemGraph, CoreGraph, CoreGraphConfig, GmemIssue, GmemPolicyConfig, GmemReject,
    GmemRejectReason, GmemRequest, GmemRequestKind, GmemStats, SmemIssue, SmemReject,
    SmemRejectReason, SmemRequest, SmemStats,
};
use crate::timeq::{Cycle, Ticket};

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreStats {
    pub gmem: GmemStats,
    pub smem: SmemStats,
}

pub struct CoreTimingModel {
    graph: CoreGraph,
    pending_gmem: Vec<VecDeque<(u64, Cycle)>>,
    pending_smem: Vec<VecDeque<(u64, Cycle)>>,
    cluster_gmem: Arc<std::sync::RwLock<ClusterGmemGraph>>,
    core_id: usize,
    cluster_id: usize,
    gmem_policy: GmemPolicyConfig,
    next_gmem_id: u64,
    logger: Arc<Logger>,
    trace: Option<TraceSink>,
    log_stats: bool,
    last_logged_gmem_completed: u64,
    last_logged_smem_completed: u64,
}

impl CoreTimingModel {
    pub fn new(
        config: CoreGraphConfig,
        num_warps: usize,
        core_id: usize,
        cluster_id: usize,
        cluster_gmem: Arc<std::sync::RwLock<ClusterGmemGraph>>,
        logger: Arc<Logger>,
    ) -> Self {
        let trace_path = env::var("CYCLOTRON_TIMING_TRACE").ok().map(PathBuf::from);
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
            trace_path,
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
        trace_path: Option<PathBuf>,
        log_stats: bool,
    ) -> Self {
        let gmem_policy = config.gmem.policy;
        let trace = trace_path.and_then(|path| match TraceSink::new(path) {
            Ok(sink) => Some(sink),
            Err(err) => {
                info!(logger, "failed to open timing trace file: {err}");
                None
            }
        });

        Self {
            graph: CoreGraph::new(config),
            pending_gmem: vec![VecDeque::new(); num_warps],
            pending_smem: vec![VecDeque::new(); num_warps],
            cluster_gmem,
            core_id,
            cluster_id,
            gmem_policy,
            next_gmem_id: 0,
            logger,
            trace,
            log_stats,
            last_logged_gmem_completed: 0,
            last_logged_smem_completed: 0,
        }
    }

    pub fn issue_gmem_request(
        &mut self,
        now: Cycle,
        warp: usize,
        mut request: GmemRequest,
        scheduler: &mut Scheduler,
    ) -> Result<Ticket, Cycle> {
        if warp >= self.pending_gmem.len() {
            return Err(now.saturating_add(1));
        }

        request.warp = warp;
        request.core_id = self.core_id;
        request.cluster_id = self.cluster_id;
        if request.id == 0 {
            request.id = self.next_gmem_id;
        }
        self.maybe_convert_mmio_flush(&mut request);
        let issue_bytes = request.bytes;
        let issue_result = {
            let mut cluster = self.cluster_gmem.write().unwrap();
            cluster.issue(self.core_id, now, request)
        };
        match issue_result {
            Ok(GmemIssue { request_id, ticket }) => {
                if request_id >= self.next_gmem_id {
                    self.next_gmem_id = request_id.saturating_add(1);
                }
                let ready_at = ticket.ready_at();
                self.add_gmem_pending(warp, request_id, ready_at, scheduler);
                self.trace_event(now, "gmem_issue", warp, Some(request_id), issue_bytes, None);
                info!(
                    self.logger,
                    "[gmem] warp {} issued request {} ready@{} bytes={}",
                    warp,
                    request_id,
                    ready_at,
                    issue_bytes
                );
                Ok(ticket)
            }
            Err(GmemReject {
                request,
                retry_at,
                reason,
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                let reason_str = match reason {
                    GmemRejectReason::Busy => "busy",
                    GmemRejectReason::QueueFull => "queue_full",
                };
                self.trace_event(
                    now,
                    "gmem_reject",
                    warp,
                    Some(request.id),
                    request.bytes,
                    Some(reason_str),
                );
                info!(
                    self.logger,
                    "[gmem] warp {} stalled ({:?}) retry@{} bytes={}",
                    warp,
                    reason,
                    wait_until,
                    request.bytes
                );
                Err(wait_until)
            }
        }
    }

    pub fn issue_smem_request(
        &mut self,
        now: Cycle,
        warp: usize,
        mut request: SmemRequest,
        scheduler: &mut Scheduler,
    ) -> Result<Ticket, Cycle> {
        if warp >= self.pending_smem.len() {
            return Err(now.saturating_add(1));
        }

        request.warp = warp;
        match self.graph.issue_smem(now, request) {
            Ok(SmemIssue { request_id, ticket }) => {
                let ready_at = ticket.ready_at();
                let size_bytes = ticket.size_bytes();
                self.add_smem_pending(warp, request_id, ready_at, scheduler);
                self.trace_event(now, "smem_issue", warp, Some(request_id), size_bytes, None);
                info!(
                    self.logger,
                    "[smem] warp {} issued request {} ready@{} bytes={}",
                    warp,
                    request_id,
                    ready_at,
                    size_bytes
                );
                Ok(ticket)
            }
            Err(SmemReject {
                request,
                retry_at,
                reason,
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                let reason_str = match reason {
                    SmemRejectReason::Busy => "busy",
                    SmemRejectReason::QueueFull => "queue_full",
                };
                self.trace_event(
                    now,
                    "smem_reject",
                    warp,
                    Some(request.id),
                    request.bytes,
                    Some(reason_str),
                );
                info!(
                    self.logger,
                    "[smem] warp {} stalled ({:?}) retry@{} bytes={}",
                    warp,
                    reason,
                    wait_until,
                    request.bytes
                );
                Err(wait_until)
            }
        }
    }

    fn maybe_convert_mmio_flush(&self, request: &mut GmemRequest) {
        if !request.kind.is_mem() || request.is_load {
            return;
        }
        let size = self.gmem_policy.l0_flush_mmio_size;
        if size == 0 {
            return;
        }
        let base = self.gmem_policy.l0_flush_mmio_base;
        let stride = self.gmem_policy.l0_flush_mmio_stride;
        let core = self.core_id as u64;
        let start = base.saturating_add(stride.saturating_mul(core));
        let end = start.saturating_add(size);
        if request.addr >= start && request.addr < end {
            request.kind = GmemRequestKind::FlushL0;
            request.stall_on_completion = true;
        }
    }

    pub fn tick(&mut self, now: Cycle, scheduler: &mut Scheduler) {
        let prev_gmem = self
            .cluster_gmem
            .read()
            .unwrap()
            .stats(self.core_id)
            .completed;
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

        for completion in gmem_completions {
            let warp = completion.request.warp;
            let completed_id = completion.request.id;
            if !self.remove_gmem_pending(warp, completed_id, scheduler) {
                continue;
            }
            self.trace_event(
                now,
                "gmem_complete",
                warp,
                Some(completed_id),
                completion.request.bytes,
                None,
            );
            info!(
                self.logger,
                "[gmem] warp {} completed request {} done@{}",
                warp,
                completed_id,
                now
            );
        }

        self.graph.tick(now);

        while let Some(completion) = self.graph.pop_smem_completion() {
            let warp = completion.request.warp;
            let completed_id = completion.request.id;
            if !self.remove_smem_pending(warp, completed_id, scheduler) {
                continue;
            }
            self.trace_event(
                now,
                "smem_complete",
                warp,
                Some(completed_id),
                completion.request.bytes,
                None,
            );
            info!(
                self.logger,
                "[smem] warp {} completed request {} done@{}", warp, completed_id, now
            );
        }

        if self.log_stats {
            if let Some(gmem_stats) = gmem_stats_snapshot {
                if gmem_stats.completed != prev_gmem {
                    info!(
                        self.logger,
                        "[gmem] stats issued={} completed={} inflight={} queue_full_rejects={} busy_rejects={} bytes_issued={} bytes_completed={}",
                        gmem_stats.issued,
                        gmem_stats.completed,
                        gmem_stats.inflight,
                        gmem_stats.queue_full_rejects,
                        gmem_stats.busy_rejects,
                        gmem_stats.bytes_issued,
                        gmem_stats.bytes_completed
                    );
                    self.last_logged_gmem_completed = gmem_stats.completed;
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
        self.pending_gmem
            .iter()
            .map(|queue| queue.len())
            .sum()
    }

    pub fn has_pending_smem(&self, warp: usize) -> bool {
        self.pending_smem
            .get(warp)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false)
    }

    pub fn outstanding_smem(&self) -> usize {
        self.pending_smem
            .iter()
            .map(|queue| queue.len())
            .sum()
    }

    pub fn stats(&self) -> CoreStats {
        let gmem_stats = self
            .cluster_gmem
            .read()
            .unwrap()
            .stats(self.core_id);
        CoreStats {
            gmem: gmem_stats,
            smem: self.graph.smem_stats(),
        }
    }

    pub fn clear_stats(&mut self) {
        self.cluster_gmem
            .write()
            .unwrap()
            .clear_stats(self.core_id);
        self.graph.clear_smem_stats();
        self.last_logged_gmem_completed = 0;
        self.last_logged_smem_completed = 0;
    }

    fn trace_event(
        &mut self,
        cycle: Cycle,
        event: &str,
        warp: usize,
        request_id: Option<u64>,
        bytes: u32,
        reason: Option<&str>,
    ) {
        if let Some(trace) = self.trace.as_mut() {
            trace.write_event(cycle, event, warp, request_id, bytes, reason);
        }
    }

    fn add_gmem_pending(
        &mut self,
        warp: usize,
        request_id: u64,
        ready_at: Cycle,
        scheduler: &mut Scheduler,
    ) {
        if let Some(slot) = self.pending_gmem.get_mut(warp) {
            slot.push_back((request_id, ready_at));
        }
        self.update_scheduler_state(warp, scheduler);
    }

    fn remove_gmem_pending(
        &mut self,
        warp: usize,
        request_id: u64,
        scheduler: &mut Scheduler,
    ) -> bool {
        if let Some(slot) = self.pending_gmem.get_mut(warp) {
            if let Some(pos) = slot.iter().position(|(id, _)| *id == request_id) {
                slot.remove(pos);
                self.update_scheduler_state(warp, scheduler);
                return true;
            }
        }
        false
    }

    fn add_smem_pending(
        &mut self,
        warp: usize,
        request_id: u64,
        ready_at: Cycle,
        scheduler: &mut Scheduler,
    ) {
        if let Some(slot) = self.pending_smem.get_mut(warp) {
            slot.push_back((request_id, ready_at));
        }
        self.update_scheduler_state(warp, scheduler);
    }

    fn remove_smem_pending(
        &mut self,
        warp: usize,
        request_id: u64,
        scheduler: &mut Scheduler,
    ) -> bool {
        if let Some(slot) = self.pending_smem.get_mut(warp) {
            if let Some(pos) = slot.iter().position(|(id, _)| *id == request_id) {
                slot.remove(pos);
                self.update_scheduler_state(warp, scheduler);
                return true;
            }
        }
        false
    }

    fn update_scheduler_state(&mut self, warp: usize, scheduler: &mut Scheduler) {
        let gmem_pending = self
            .pending_gmem
            .get(warp)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false);
        let smem_pending = self
            .pending_smem
            .get(warp)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false);
        if !gmem_pending && !smem_pending {
            scheduler.clear_resource_wait(warp);
        }
    }
}

struct TraceSink {
    writer: BufWriter<File>,
    wrote_header: bool,
}

impl TraceSink {
    fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            wrote_header: false,
        })
    }

    fn write_event(
        &mut self,
        cycle: Cycle,
        event: &str,
        warp: usize,
        request_id: Option<u64>,
        bytes: u32,
        reason: Option<&str>,
    ) {
        if !self.wrote_header {
            let _ = writeln!(self.writer, "cycle,event,warp,request_id,bytes,reason");
            self.wrote_header = true;
        }

        let req_id_str = request_id
            .map(|id| id.to_string())
            .unwrap_or_else(String::new);
        let reason_str = reason.unwrap_or("");
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{},{}",
            cycle, event, warp, req_id_str, bytes, reason_str
        );
    }
}

impl Drop for TraceSink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muon::config::{LaneConfig, MuonConfig};
    use crate::muon::scheduler::Scheduler;
    use crate::sim::log::Logger;
    use crate::timeflow::{ClusterGmemGraph, CoreGraphConfig, GmemFlowConfig, SmemFlowConfig};
    use crate::timeq::{module_now, ServerConfig};
    use std::sync::Arc;

    fn make_scheduler(num_warps: usize) -> Scheduler {
        let mut config = MuonConfig::default();
        config.num_warps = num_warps;
        config.lane_config = LaneConfig::default();
        Scheduler::new(Arc::new(config), 0)
    }

    fn make_model(num_warps: usize) -> CoreTimingModel {
        let mut gmem = GmemFlowConfig::default();
        gmem.nodes.coalescer.queue_capacity = 1;
        gmem.nodes.l0d_tag.queue_capacity = 1;
        gmem.nodes.dram.queue_capacity = 1;
        gmem.links.default.entries = 1;
        let smem = SmemFlowConfig {
            lane: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            num_banks: 1,
            num_lanes: 1,
            link_capacity: 1,
        };
        let cfg = CoreGraphConfig { gmem, smem };
        let logger = Arc::new(Logger::silent());
        let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
            cfg.gmem.clone(),
            1,
            1,
        )));
        CoreTimingModel::new(cfg, num_warps, 0, 0, cluster_gmem, logger)
    }

    #[test]
    fn accepted_request_marks_pending_until_completion() {
        let mut scheduler = make_scheduler(1);
        scheduler.spawn_single_warp();

        let mut model = make_model(1);
        let now = module_now(&scheduler);
        let request = GmemRequest::new(0, 16, 0xF, true);
        let ticket = model
            .issue_gmem_request(now, 0, request, &mut scheduler)
            .expect("request should accept");
        assert!(model.has_pending_gmem(0));
        let stats = model.stats();
        assert_eq!(stats.gmem.issued, 1);

        // Advance time until the request matures.
        let ready_at = ticket.ready_at();
        let mut cycle = now;
        let mut released = false;
        let max_cycles = 500;
        for _ in 0..max_cycles {
            model.tick(cycle, &mut scheduler);
            if !model.has_pending_gmem(0) {
                released = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            released,
            "GMEM request did not complete within {} cycles (ready_at={}, outstanding={})",
            max_cycles,
            ready_at,
            model.outstanding_gmem()
        );
        assert!(!model.has_pending_gmem(0));
        assert_eq!(model.stats().gmem.completed, 1);
    }

    #[test]
    fn queue_full_schedules_retry_and_replay() {
        let mut scheduler = make_scheduler(2);
        let threads = vec![vec![(0, 0, 0)], vec![(0, 0, 1)]];
        scheduler.spawn_n_warps(0x8000_0000, &threads);

        let mut model = make_model(2);
        let now = module_now(&scheduler);
        let request0 = GmemRequest::new(0, 16, 0xF, true);
        model
            .issue_gmem_request(now, 0, request0, &mut scheduler)
            .expect("first request should accept");

        let mut request1 = GmemRequest::new(1, 16, 0xF, true);
        request1.addr = 128;
        let retry_at = model
            .issue_gmem_request(now, 1, request1, &mut scheduler)
            .expect_err("second request should stall");
        assert!(retry_at > now);
        assert!(scheduler.get_schedule(1).is_none());
        let stats = model.stats();
        assert_eq!(stats.gmem.queue_full_rejects + stats.gmem.busy_rejects, 1);
    }

    #[test]
    fn smem_request_tracks_pending_and_stats() {
        let mut scheduler = make_scheduler(1);
        scheduler.spawn_single_warp();

        let mut model = make_model(1);
        let now = module_now(&scheduler);
        let request = SmemRequest::new(0, 32, 0xF, false, 0);
        let ticket = model
            .issue_smem_request(now, 0, request, &mut scheduler)
            .expect("smem request should accept");
        assert!(model.has_pending_smem(0));

        let ready_at = ticket.ready_at();
        let mut cycle = now;
        let mut released = false;
        for _ in 0..200 {
            model.tick(cycle, &mut scheduler);
            if !model.has_pending_smem(0) {
                released = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            released,
            "SMEM request did not complete within 200 cycles (ready_at={}, outstanding={})",
            ready_at,
            model.outstanding_smem()
        );
        let stats = model.stats();
        assert_eq!(stats.smem.completed, 1);
    }

    #[test]
    fn smem_crossbar_capacity_backpressures_new_requests() {
        let mut scheduler = make_scheduler(2);
        let threads = vec![vec![(0, 0, 0)], vec![(0, 0, 1)]];
        scheduler.spawn_n_warps(0x8000_0000, &threads);

        let gmem = GmemFlowConfig::default();
        let smem = SmemFlowConfig {
            lane: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            num_banks: 2,
            num_lanes: 1,
            link_capacity: 1,
        };
        let cfg = CoreGraphConfig { gmem, smem };
        let logger = Arc::new(Logger::silent());
        let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
            cfg.gmem.clone(),
            1,
            1,
        )));
        let mut model = CoreTimingModel::new(cfg, 2, 0, 0, cluster_gmem, logger);

        let now = module_now(&scheduler);
        let req0 = SmemRequest::new(0, 32, 0xF, false, 0);
        model
            .issue_smem_request(now, 0, req0, &mut scheduler)
            .expect("first SMEM request should be accepted");

        let req1 = SmemRequest::new(1, 32, 0xF, false, 1);
        let retry_at = model
            .issue_smem_request(now, 1, req1, &mut scheduler)
            .expect_err("second SMEM request should backpressure on crossbar");
        assert!(
            retry_at > now,
            "retry cycle should be strictly in the future (now={}, retry_at={})",
            now,
            retry_at
        );

        let stats = model.stats().smem;
        assert!(
            stats.queue_full_rejects + stats.busy_rejects >= 1,
            "expected at least one rejection to be counted (queue_full={}, busy={})",
            stats.queue_full_rejects,
            stats.busy_rejects
        );
    }
}
