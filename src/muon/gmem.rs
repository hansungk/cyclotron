use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::info;
use crate::muon::scheduler::Scheduler;
use crate::sim::log::Logger;
use crate::timeflow::{
    CoreGraph, CoreGraphConfig, GmemIssue, GmemReject, GmemRejectReason, GmemRequest, GmemStats,
    SmemIssue, SmemReject, SmemRejectReason, SmemRequest, SmemStats,
};
use crate::timeq::{Cycle, Ticket};

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreStats {
    pub gmem: GmemStats,
    pub smem: SmemStats,
}

pub struct CoreTimingModel {
    graph: CoreGraph,
    pending_gmem: Vec<Option<(u64, Cycle)>>,
    pending_smem: Vec<Option<(u64, Cycle)>>,
    logger: Arc<Logger>,
    trace: Option<TraceSink>,
    log_stats: bool,
    last_logged_gmem_completed: u64,
    last_logged_smem_completed: u64,
}

impl CoreTimingModel {
    pub fn new(config: CoreGraphConfig, num_warps: usize, logger: Arc<Logger>) -> Self {
        let trace_path = env::var("CYCLOTRON_TIMING_TRACE").ok().map(PathBuf::from);
        let log_stats = env::var("CYCLOTRON_TIMING_LOG_STATS")
            .ok()
            .map(|val| {
                let lowered = val.to_ascii_lowercase();
                lowered == "1" || lowered == "true" || lowered == "yes"
            })
            .unwrap_or(false);
        Self::with_options(config, num_warps, logger, trace_path, log_stats)
    }

    pub fn with_options(
        config: CoreGraphConfig,
        num_warps: usize,
        logger: Arc<Logger>,
        trace_path: Option<PathBuf>,
        log_stats: bool,
    ) -> Self {
        let trace = trace_path.and_then(|path| match TraceSink::new(path) {
            Ok(sink) => Some(sink),
            Err(err) => {
                info!(logger, "failed to open timing trace file: {err}");
                None
            }
        });

        Self {
            graph: CoreGraph::new(config),
            pending_gmem: vec![None; num_warps],
            pending_smem: vec![None; num_warps],
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

        if self.pending_gmem[warp].is_some() {
            let wait_until = now.saturating_add(1);
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            info!(
                self.logger,
                "[gmem] warp {} has outstanding request, retry@{}", warp, wait_until
            );
            self.trace_event(
                now,
                "gmem_reject_outstanding",
                warp,
                None,
                request.bytes,
                Some("outstanding"),
            );
            return Err(wait_until);
        }

        request.warp = warp;
        let issue_bytes = request.bytes;
        match self.graph.issue_gmem(now, request) {
            Ok(GmemIssue { request_id, ticket }) => {
                let ready_at = ticket.ready_at();
                self.set_gmem_pending(warp, Some((request_id, ready_at)), scheduler);
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

        if self.pending_smem[warp].is_some() {
            let wait_until = now.saturating_add(1);
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            self.trace_event(
                now,
                "smem_reject_outstanding",
                warp,
                None,
                request.bytes,
                Some("outstanding"),
            );
            info!(
                self.logger,
                "[smem] warp {} has outstanding request, retry@{}", warp, wait_until
            );
            return Err(wait_until);
        }

        request.warp = warp;
        match self.graph.issue_smem(now, request) {
            Ok(SmemIssue { request_id, ticket }) => {
                let ready_at = ticket.ready_at();
                let size_bytes = ticket.size_bytes();
                self.set_smem_pending(warp, Some((request_id, ready_at)), scheduler);
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

    pub fn tick(&mut self, now: Cycle, scheduler: &mut Scheduler) {
        let prev_gmem = self.graph.gmem_stats().completed;
        let prev_smem = self.graph.smem_stats().completed;
        self.graph.tick(now);

        while let Some(completion) = self.graph.pop_gmem_completion() {
            let warp = completion.request.warp;
            let completed_id = completion.request.id;
            if matches!(self.pending_gmem.get(warp).copied().flatten(), Some((id, _)) if id == completed_id)
            {
                self.set_gmem_pending(warp, None, scheduler);
            } else {
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
                "[gmem] warp {} completed request {} done@{}", warp, completed_id, now
            );
        }

        while let Some(completion) = self.graph.pop_smem_completion() {
            let warp = completion.request.warp;
            let completed_id = completion.request.id;
            if matches!(self.pending_smem.get(warp).copied().flatten(), Some((id, _)) if id == completed_id)
            {
                self.set_smem_pending(warp, None, scheduler);
            } else {
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
            let gmem_stats = self.graph.gmem_stats();
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
        self.pending_gmem.get(warp).and_then(|slot| *slot).is_some()
    }

    pub fn outstanding_gmem(&self) -> usize {
        self.pending_gmem
            .iter()
            .filter(|entry| entry.is_some())
            .count()
    }

    pub fn has_pending_smem(&self, warp: usize) -> bool {
        self.pending_smem.get(warp).and_then(|slot| *slot).is_some()
    }

    pub fn outstanding_smem(&self) -> usize {
        self.pending_smem
            .iter()
            .filter(|entry| entry.is_some())
            .count()
    }

    pub fn stats(&self) -> CoreStats {
        CoreStats {
            gmem: self.graph.gmem_stats(),
            smem: self.graph.smem_stats(),
        }
    }

    pub fn clear_stats(&mut self) {
        self.graph.clear_gmem_stats();
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

    fn set_gmem_pending(
        &mut self,
        warp: usize,
        entry: Option<(u64, Cycle)>,
        scheduler: &mut Scheduler,
    ) {
        if let Some(slot) = self.pending_gmem.get_mut(warp) {
            *slot = entry;
        }
        self.update_scheduler_state(warp, scheduler);
    }

    fn set_smem_pending(
        &mut self,
        warp: usize,
        entry: Option<(u64, Cycle)>,
        scheduler: &mut Scheduler,
    ) {
        if let Some(slot) = self.pending_smem.get_mut(warp) {
            *slot = entry;
        }
        self.update_scheduler_state(warp, scheduler);
    }

    fn earliest_ready(&self, warp: usize) -> Option<Cycle> {
        let mut earliest = None;
        if let Some((_, ready)) = self.pending_gmem.get(warp).copied().flatten() {
            earliest = Some(ready);
        }
        if let Some((_, ready)) = self.pending_smem.get(warp).copied().flatten() {
            earliest = Some(match earliest {
                Some(curr) => curr.min(ready),
                None => ready,
            });
        }
        earliest
    }

    fn update_scheduler_state(&mut self, warp: usize, scheduler: &mut Scheduler) {
        let gmem_pending = self.pending_gmem.get(warp).and_then(|x| *x).is_some();
        let smem_pending = self.pending_smem.get(warp).and_then(|x| *x).is_some();
        if !gmem_pending && !smem_pending {
            scheduler.set_resource_pending(warp, false);
            scheduler.clear_resource_wait(warp);
        } else {
            scheduler.set_resource_pending(warp, true);
            if let Some(deadline) = self.earliest_ready(warp) {
                scheduler.set_resource_wait_until(warp, Some(deadline));
            }
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
    use crate::timeflow::{CoreGraphConfig, GmemFlowConfig, SmemFlowConfig};
    use crate::timeq::{module_now, ServerConfig};
    use std::sync::Arc;

    fn make_scheduler(num_warps: usize) -> Scheduler {
        let mut config = MuonConfig::default();
        config.num_warps = num_warps;
        config.lane_config = LaneConfig::default();
        Scheduler::new(Arc::new(config), 0)
    }

    fn make_model(num_warps: usize) -> CoreTimingModel {
        let gmem = GmemFlowConfig {
            coalescer: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            cache: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            dram: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            link_capacity: 1,
        };
        let smem = SmemFlowConfig {
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            num_banks: 1,
            link_capacity: 1,
        };
        let cfg = CoreGraphConfig { gmem, smem };
        let logger = Arc::new(Logger::silent());
        CoreTimingModel::new(cfg, num_warps, logger)
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
        assert!(scheduler.resource_pending(0));
        assert!(model.has_pending_gmem(0));
        let stats = model.stats();
        assert_eq!(stats.gmem.issued, 1);

        // Advance time until the request matures.
        let ready_at = ticket.ready_at();
        let mut cycle = now;
        let mut released = false;
        for _ in 0..200 {
            model.tick(cycle, &mut scheduler);
            if !scheduler.resource_pending(0) {
                released = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            released,
            "GMEM request did not complete within 200 cycles (ready_at={}, outstanding={})",
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

        let request1 = GmemRequest::new(1, 16, 0xF, true);
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

        let gmem = GmemFlowConfig {
            coalescer: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            cache: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            dram: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            link_capacity: 2,
        };
        let smem = SmemFlowConfig {
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            num_banks: 2,
            link_capacity: 1,
        };
        let cfg = CoreGraphConfig { gmem, smem };
        let logger = Arc::new(Logger::silent());
        let mut model = CoreTimingModel::new(cfg, 2, logger);

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
