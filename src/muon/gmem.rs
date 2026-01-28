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
    BarrierManager, ClusterGmemGraph, CoreGraph, CoreGraphConfig, DmaQueue, FenceQueue, FenceRequest,
    GmemPolicyConfig, GmemReject, GmemRequest, GmemRequestKind, GmemStats, IcacheIssue,
    IcacheReject, IcacheRequest, IcacheSubgraph, LsuIssue, LsuReject, LsuRejectReason, LsuSubgraph,
    OperandFetchQueue, SmemFlowConfig, SmemIssue, SmemReject, SmemRequest, SmemStats, TensorQueue,
    WritebackPayload, WritebackQueue,
};
use crate::timeflow::lsu::LsuPayload;
use crate::timeq::{Cycle, Ticket};

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreStats {
    pub gmem: GmemStats,
    pub smem: SmemStats,
}

pub struct CoreTimingModel {
    graph: CoreGraph,
    lsu: LsuSubgraph,
    icache: IcacheSubgraph,
    operand_fetch: OperandFetchQueue,
    writeback: WritebackQueue,
    pending_writeback: VecDeque<WritebackPayload>,
    barrier: BarrierManager,
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
    log_stats: bool,
    last_logged_gmem_completed: u64,
    last_logged_smem_completed: u64,
}

struct PendingClusterIssue<T> {
    request: T,
    retry_at: Cycle,
}

#[derive(Clone, Copy)]
struct IcacheInflight {
    ready_at: Cycle,
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
        let lsu = LsuSubgraph::new(config.lsu.clone(), num_warps);
        let gmem_policy = config.gmem.policy;
        let smem_config = config.smem.clone();
        let icache = IcacheSubgraph::new(config.icache.clone());
        let operand_fetch = OperandFetchQueue::new(config.operand_fetch.clone());
        let writeback = WritebackQueue::new(config.writeback.clone());
        let barrier = BarrierManager::new(config.barrier.clone(), num_warps);
        let fence = FenceQueue::new(config.fence.clone());
        let dma = DmaQueue::new(config.dma.clone());
        let tensor = TensorQueue::new(config.tensor.clone());
        let trace = trace_path.and_then(|path| match TraceSink::new(path) {
            Ok(sink) => Some(sink),
            Err(err) => {
                info!(logger, "failed to open timing trace file: {err}");
                None
            }
        });

        Self {
            graph: CoreGraph::new(config),
            lsu,
            icache,
            operand_fetch,
            writeback,
            pending_writeback: VecDeque::new(),
            barrier,
            fence,
            pending_fence: VecDeque::new(),
            fence_inflight: vec![None; num_warps],
            dma,
            tensor,
            icache_inflight: vec![None; num_warps],
            pending_cluster_gmem: VecDeque::new(),
            pending_cluster_smem: VecDeque::new(),
            pending_gmem: vec![VecDeque::new(); num_warps],
            pending_smem: vec![VecDeque::new(); num_warps],
            cluster_gmem,
            core_id,
            cluster_id,
            gmem_policy,
            smem_config,
            next_gmem_id: 0,
            next_smem_id: 0,
            next_icache_id: 0,
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
            request.id = if self.next_gmem_id == 0 {
                1
            } else {
                self.next_gmem_id
            };
        }
        self.maybe_convert_mmio_flush(&mut request);
        if request.kind.is_mem() {
            if let Some(lane_addrs) = request.lane_addrs.as_ref() {
                let line_bytes = self.gmem_policy.l1_line_bytes.max(1) as u64;
                let mut lines: Vec<u64> = lane_addrs
                    .iter()
                    .map(|addr| (addr / line_bytes) * line_bytes)
                    .collect();
                lines.sort_unstable();
                lines.dedup();
                if !lines.is_empty() {
                    request.coalesced_lines = Some(lines);
                }
            }
        }
        request.lane_addrs = None;
        let issue_bytes = request.bytes;
        let request_id = request.id;
        if request_id >= self.next_gmem_id {
            self.next_gmem_id = request_id.saturating_add(1);
        }
        let split_count = request
            .coalesced_lines
            .as_ref()
            .map(|lines| lines.len().max(1))
            .unwrap_or(1);
        let is_flush = request.kind.is_flush_l0() || request.kind.is_flush_l1();
        if let Err(reject) = self.operand_fetch.try_issue(now, request.bytes) {
            let wait_until = reject.retry_at.max(now.saturating_add(1));
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            return Err(wait_until);
        }
        let issue_result = self.lsu.issue_gmem(now, request);
        match issue_result {
            Ok(LsuIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                self.add_gmem_pending(warp, request_id, ready_at, scheduler, split_count);
                if is_flush {
                    self.register_fence(warp, request_id, scheduler);
                }
                self.trace_event(now, "gmem_issue", warp, Some(request_id), issue_bytes, None);
                info!(
                    self.logger,
                    "[lsu] warp {} accepted gmem request {} ready@{} bytes={}",
                    warp,
                    request_id,
                    ready_at,
                    issue_bytes
                );
                Ok(ticket)
            }
            Err(LsuReject {
                request,
                retry_at,
                reason,
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                let reason_str = match reason {
                    LsuRejectReason::Busy => "busy",
                    LsuRejectReason::QueueFull => "queue_full",
                };
                let (request_id, request_bytes) = match request {
                    LsuPayload::Gmem(req) => (req.id, req.bytes),
                    LsuPayload::Smem(req) => (req.id, req.bytes),
                };
                self.trace_event(
                    now,
                    "gmem_reject",
                    warp,
                    Some(request_id),
                    request_bytes,
                    Some(reason_str),
                );
                info!(
                    self.logger,
                    "[lsu] warp {} stalled ({:?}) retry@{} bytes={}",
                    warp,
                    reason,
                    wait_until,
                    request_bytes
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
        if request.id == 0 {
            request.id = self.next_smem_id;
            self.next_smem_id = self.next_smem_id.saturating_add(1);
        } else if request.id >= self.next_smem_id {
            self.next_smem_id = request.id.saturating_add(1);
        }
        let request_id = request.id;
        let split_count = self.split_smem_request(&request).len().max(1);
        let issue_bytes = request.bytes;
        if let Err(reject) = self.operand_fetch.try_issue(now, request.bytes) {
            let wait_until = reject.retry_at.max(now.saturating_add(1));
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            return Err(wait_until);
        }
        let issue_result = self.lsu.issue_smem(now, request);
        match issue_result {
            Ok(LsuIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                self.add_smem_pending(warp, request_id, ready_at, scheduler, split_count);
                self.trace_event(now, "smem_issue", warp, None, issue_bytes, None);
                info!(
                    self.logger,
                    "[lsu] warp {} accepted smem request ready@{} bytes={}",
                    warp,
                    ready_at,
                    issue_bytes
                );
                Ok(ticket)
            }
            Err(LsuReject {
                request,
                retry_at,
                reason,
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                let reason_str = match reason {
                    LsuRejectReason::Busy => "busy",
                    LsuRejectReason::QueueFull => "queue_full",
                };
                let (request_id, request_bytes) = match request {
                    LsuPayload::Smem(req) => (req.id, req.bytes),
                    LsuPayload::Gmem(req) => (req.id, req.bytes),
                };
                self.trace_event(
                    now,
                    "smem_reject",
                    warp,
                    Some(request_id),
                    request_bytes,
                    Some(reason_str),
                );
                info!(
                    self.logger,
                    "[lsu] warp {} stalled ({:?}) retry@{} bytes={}",
                    warp,
                    reason,
                    wait_until,
                    request_bytes
                );
                Err(wait_until)
            }
        }
    }

    pub fn issue_dma(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, Cycle> {
        match self.dma.try_issue(now, bytes) {
            Ok(issue) => Ok(issue.ticket),
            Err(reject) => Err(reject.retry_at),
        }
    }

    pub fn issue_tensor(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, Cycle> {
        match self.tensor.try_issue(now, bytes) {
            Ok(issue) => Ok(issue.ticket),
            Err(reject) => Err(reject.retry_at),
        }
    }

    pub fn allow_fetch(
        &mut self,
        now: Cycle,
        warp: usize,
        pc: u32,
        scheduler: &mut Scheduler,
    ) -> bool {
        if warp >= self.icache_inflight.len() {
            return true;
        }

        if let Some(entry) = self.icache_inflight[warp].as_ref() {
            if now >= entry.ready_at {
                self.icache_inflight[warp] = None;
                return true;
            }
            scheduler.set_resource_wait_until(warp, Some(entry.ready_at));
            scheduler.replay_instruction(warp);
            return false;
        }

        let mut request = IcacheRequest::new(warp, pc, 8);
        request.core_id = self.core_id;
        if request.id == 0 {
            request.id = if self.next_icache_id == 0 {
                1
            } else {
                self.next_icache_id
            };
        }
        if request.id >= self.next_icache_id {
            self.next_icache_id = request.id.saturating_add(1);
        }

        match self.icache.issue(now, request) {
            Ok(IcacheIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                if ready_at <= now {
                    true
                } else {
                    self.icache_inflight[warp] = Some(IcacheInflight {
                        ready_at,
                    });
                    scheduler.set_resource_wait_until(warp, Some(ready_at));
                    scheduler.replay_instruction(warp);
                    false
                }
            }
            Err(IcacheReject {
                retry_at,
                reason: _,
                ..
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                false
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
        self.icache.tick(now);
        self.lsu.tick(now);
        self.operand_fetch.tick(now);
        self.dma.tick(now);
        self.tensor.tick(now);
        if let Some(released) = self.barrier.tick(now) {
            for warp in released {
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

        self.graph.tick(now);
        let mut smem_completions = Vec::new();
        while let Some(completion) = self.graph.pop_smem_completion() {
            smem_completions.push(completion);
        }

        self.drain_pending_writeback(now);
        self.drain_pending_fence(now);

        for completion in gmem_completions {
            let is_flush = completion.request.kind.is_flush_l0()
                || completion.request.kind.is_flush_l1();
            if is_flush {
                self.handle_gmem_completion(now, completion.clone(), scheduler);
                self.enqueue_fence(
                    now,
                    FenceRequest {
                        warp: completion.request.warp,
                        request_id: completion.request.id,
                    },
                );
                continue;
            }
            self.enqueue_writeback(now, WritebackPayload::Gmem(completion));
        }

        for completion in smem_completions {
            self.enqueue_writeback(now, WritebackPayload::Smem(completion));
        }

        self.writeback.tick(now);
        self.fence.tick(now);

        while let Some(payload) = self.writeback.pop_ready() {
            match payload {
                WritebackPayload::Gmem(completion) => {
                    self.handle_gmem_completion(now, completion, scheduler);
                }
                WritebackPayload::Smem(completion) => {
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

    fn drive_lsu_issues(&mut self, now: Cycle) {
        loop {
            let payload = match self.lsu.peek_ready(now) {
                Some(payload) => payload,
                None => break,
            };

            match &payload {
                LsuPayload::Gmem(req) => {
                    let split = self.split_gmem_request(req);
                    for child in split {
                        self.pending_cluster_gmem.push_back(PendingClusterIssue {
                            request: child,
                            retry_at: now,
                        });
                    }
                }
                LsuPayload::Smem(req) => {
                    let split = self.split_smem_request(req);
                    for child in split {
                        self.pending_cluster_smem.push_back(PendingClusterIssue {
                            request: child,
                            retry_at: now,
                        });
                    }
                }
            }

            if let Some(completion) = self.lsu.take_ready(now) {
                self.lsu.release_issue_resources(&completion.request);
            }
        }
    }

    fn split_gmem_request(&self, request: &GmemRequest) -> Vec<GmemRequest> {
        if !request.kind.is_mem() {
            return vec![request.clone()];
        }
        let line_bytes = self.gmem_policy.l1_line_bytes.max(1) as u64;
        let lines = request
            .coalesced_lines
            .clone()
            .unwrap_or_else(|| vec![(request.addr / line_bytes) * line_bytes]);
        if lines.is_empty() {
            return vec![request.clone()];
        }
        lines
            .into_iter()
            .map(|line| {
                let mut child = request.clone();
                child.addr = line;
                child.line_addr = line;
                child.bytes = line_bytes as u32;
                child.coalesced_lines = None;
                child.lane_addrs = None;
                child
            })
            .collect()
    }

    fn split_smem_request(&self, request: &SmemRequest) -> Vec<SmemRequest> {
        let num_banks = self.smem_config.num_banks.max(1) as u64;
        let num_subbanks = self.smem_config.num_subbanks.max(1) as u64;
        let word_bytes = self.smem_config.word_bytes.max(1) as u64;
        let active = request.active_lanes.max(1);
        let bytes_per_lane = request.bytes.saturating_div(active).max(1);

        if let Some(lane_addrs) = request.lane_addrs.as_ref() {
            let mut groups: std::collections::HashMap<(usize, usize), (u32, u64)> =
                std::collections::HashMap::new();
            for &addr in lane_addrs {
                let word = addr / word_bytes;
                let bank = (word % num_banks) as usize;
                let subbank = ((word / num_banks) % num_subbanks) as usize;
                let entry = groups.entry((bank, subbank)).or_insert((0, addr));
                entry.0 = entry.0.saturating_add(1);
            }
            if groups.is_empty() {
                return vec![request.clone()];
            }
            return groups
                .into_iter()
                .map(|((bank, subbank), (lanes, addr))| {
                    let mut child = request.clone();
                    child.bank = bank;
                    child.subbank = subbank;
                    child.addr = addr;
                    child.active_lanes = lanes;
                    child.bytes = bytes_per_lane.saturating_mul(lanes).max(1);
                    child.lane_addrs = None;
                    child
                })
                .collect();
        }

        let word = request.addr / word_bytes;
        let bank = (word % num_banks) as usize;
        let subbank = ((word / num_banks) % num_subbanks) as usize;
        let mut child = request.clone();
        child.bank = bank;
        child.subbank = subbank;
        child.lane_addrs = None;
        vec![child]
    }

    fn issue_pending_cluster_gmem(&mut self, now: Cycle) {
        if self.pending_cluster_gmem.is_empty() {
            return;
        }

        let mut pending = VecDeque::with_capacity(self.pending_cluster_gmem.len());
        let mut remaining = self.pending_cluster_gmem.len();
        while remaining > 0 {
            remaining -= 1;
            let entry = match self.pending_cluster_gmem.pop_front() {
                Some(entry) => entry,
                None => break,
            };
            if entry.retry_at > now {
                pending.push_back(entry);
                continue;
            }

            if !self.lsu.can_reserve_load_data(&LsuPayload::Gmem(entry.request.clone())) {
                pending.push_back(PendingClusterIssue {
                    request: entry.request,
                    retry_at: now.saturating_add(1),
                });
                continue;
            }

            let issue = {
                let mut cluster = self.cluster_gmem.write().unwrap();
                cluster.issue(self.core_id, now, entry.request.clone())
            };
            match issue {
                Ok(_) => {
                    let _ = self
                        .lsu
                        .reserve_load_data(&LsuPayload::Gmem(entry.request));
                }
                Err(GmemReject { request, retry_at, .. }) => {
                    pending.push_back(PendingClusterIssue {
                        request,
                        retry_at: retry_at.max(now.saturating_add(1)),
                    });
                }
            }
        }

        self.pending_cluster_gmem = pending;
    }

    fn issue_pending_cluster_smem(&mut self, now: Cycle) {
        if self.pending_cluster_smem.is_empty() {
            return;
        }

        let mut pending = VecDeque::with_capacity(self.pending_cluster_smem.len());
        let mut remaining = self.pending_cluster_smem.len();
        while remaining > 0 {
            remaining -= 1;
            let entry = match self.pending_cluster_smem.pop_front() {
                Some(entry) => entry,
                None => break,
            };
            if entry.retry_at > now {
                pending.push_back(entry);
                continue;
            }

            if !self
                .lsu
                .can_reserve_load_data(&LsuPayload::Smem(entry.request.clone()))
            {
                pending.push_back(PendingClusterIssue {
                    request: entry.request,
                    retry_at: now.saturating_add(1),
                });
                continue;
            }

            match self.graph.issue_smem(now, entry.request.clone()) {
                Ok(SmemIssue { .. }) => {
                    let _ = self
                        .lsu
                        .reserve_load_data(&LsuPayload::Smem(entry.request));
                }
                Err(SmemReject { request, retry_at, .. }) => {
                    pending.push_back(PendingClusterIssue {
                        request,
                        retry_at: retry_at.max(now.saturating_add(1)),
                    });
                }
            }
        }

        self.pending_cluster_smem = pending;
    }

    fn drain_pending_writeback(&mut self, now: Cycle) {
        let mut remaining = VecDeque::new();
        while let Some(payload) = self.pending_writeback.pop_front() {
            if self.writeback.try_issue(now, payload.clone()).is_err() {
                remaining.push_back(payload);
                remaining.extend(self.pending_writeback.drain(..));
                break;
            }
        }
        self.pending_writeback = remaining;
    }

    fn enqueue_writeback(&mut self, now: Cycle, payload: WritebackPayload) {
        if self.writeback.try_issue(now, payload.clone()).is_err() {
            self.pending_writeback.push_back(payload);
        }
    }

    fn drain_pending_fence(&mut self, now: Cycle) {
        let mut remaining = VecDeque::new();
        while let Some(request) = self.pending_fence.pop_front() {
            if self.fence.try_issue(now, request.clone()).is_err() {
                remaining.push_back(request);
                remaining.extend(self.pending_fence.drain(..));
                break;
            }
        }
        self.pending_fence = remaining;
    }

    fn enqueue_fence(&mut self, now: Cycle, request: FenceRequest) {
        if self.fence.try_issue(now, request.clone()).is_err() {
            self.pending_fence.push_back(request);
        }
    }

    fn add_gmem_pending(
        &mut self,
        warp: usize,
        request_id: u64,
        ready_at: Cycle,
        scheduler: &mut Scheduler,
        count: usize,
    ) {
        if let Some(slot) = self.pending_gmem.get_mut(warp) {
            let repeats = count.max(1);
            for _ in 0..repeats {
                slot.push_back((request_id, ready_at));
            }
        }
        self.update_scheduler_state(warp, scheduler);
    }

    fn register_fence(&mut self, warp: usize, request_id: u64, scheduler: &mut Scheduler) {
        if !self.fence.is_enabled() {
            return;
        }
        if let Some(slot) = self.fence_inflight.get_mut(warp) {
            *slot = Some(request_id);
        }
        scheduler.set_resource_wait_until(warp, Some(Cycle::MAX));
    }

    fn handle_gmem_completion(
        &mut self,
        now: Cycle,
        completion: crate::timeflow::GmemCompletion,
        scheduler: &mut Scheduler,
    ) {
        let warp = completion.request.warp;
        let completed_id = completion.request.id;
        if !self.remove_gmem_pending(warp, completed_id, scheduler) {
            return;
        }
        self.lsu
            .release_load_data(&LsuPayload::Gmem(completion.request.clone()));
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

    fn handle_smem_completion(
        &mut self,
        now: Cycle,
        completion: crate::timeflow::SmemCompletion,
        scheduler: &mut Scheduler,
    ) {
        let warp = completion.request.warp;
        let completed_id = completion.request.id;
        if !self.remove_smem_pending(warp, completed_id, scheduler) {
            return;
        }
        self.lsu
            .release_load_data(&LsuPayload::Smem(completion.request.clone()));
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
            "[smem] warp {} completed request {} done@{}",
            warp,
            completed_id,
            now
        );
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
        count: usize,
    ) {
        if let Some(slot) = self.pending_smem.get_mut(warp) {
            let repeats = count.max(1);
            for _ in 0..repeats {
                slot.push_back((request_id, ready_at));
            }
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
        let icache_pending = self
            .icache_inflight
            .get(warp)
            .map(|entry| entry.is_some())
            .unwrap_or(false);
        let fence_pending = self
            .fence_inflight
            .get(warp)
            .map(|entry| entry.is_some())
            .unwrap_or(false);
        if !gmem_pending && !smem_pending && !icache_pending && !fence_pending {
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

    fn make_model_with_lsu(num_warps: usize, lsu_depth: usize) -> CoreTimingModel {
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
            serial: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            subbank: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            num_banks: 1,
            num_lanes: 1,
            num_subbanks: 1,
            word_bytes: 4,
            serialize_cores: false,
            link_capacity: 1,
        };
        let mut cfg = CoreGraphConfig {
            gmem,
            smem,
            ..CoreGraphConfig::default()
        };
        cfg.lsu.queues.global_ldq.queue_capacity = lsu_depth.max(1);
        cfg.lsu.queues.global_stq.queue_capacity = lsu_depth.max(1);
        cfg.lsu.queues.shared_ldq.queue_capacity = lsu_depth.max(1);
        cfg.lsu.queues.shared_stq.queue_capacity = lsu_depth.max(1);
        let logger = Arc::new(Logger::silent());
        let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
            cfg.gmem.clone(),
            1,
            1,
        )));
        CoreTimingModel::new(cfg, num_warps, 0, 0, cluster_gmem, logger)
    }

    fn make_model(num_warps: usize) -> CoreTimingModel {
        make_model_with_lsu(num_warps, 8)
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

        // Advance time until the request matures.
        let ready_at = ticket.ready_at();
        let mut cycle = now;
        let mut completed = false;
        let max_cycles = 500;
        for _ in 0..max_cycles {
            model.tick(cycle, &mut scheduler);
            if model.stats().gmem.completed >= 1 {
                completed = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            completed,
            "GMEM request did not complete within {} cycles (ready_at={}, outstanding={})",
            max_cycles,
            ready_at,
            model.outstanding_gmem()
        );
        assert!(!model.has_pending_gmem(0));
        assert_eq!(model.stats().gmem.completed, 1);
        assert!(model.stats().gmem.issued >= 1);
    }

    #[test]
    fn gmem_coalescing_adds_multiple_pending_entries() {
        let mut scheduler = make_scheduler(1);
        scheduler.spawn_single_warp();

        let mut model = make_model(1);
        let now = module_now(&scheduler);
        let mut request = GmemRequest::new(0, 16, 0x3, true);
        request.lane_addrs = Some(vec![0, 32]);
        model
            .issue_gmem_request(now, 0, request, &mut scheduler)
            .expect("coalesced request should accept");

        assert_eq!(model.outstanding_gmem(), 2);

        let max_cycles = 1000;
        let mut cycle = now;
        let mut completed = false;
        for _ in 0..max_cycles {
            model.tick(cycle, &mut scheduler);
            if model.outstanding_gmem() == 0 {
                completed = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            completed,
            "coalesced GMEM request did not complete within {} cycles (outstanding={})",
            max_cycles,
            model.outstanding_gmem()
        );
    }

    #[test]
    fn queue_full_schedules_retry_and_replay() {
        let mut scheduler = make_scheduler(1);
        scheduler.spawn_single_warp();

        let mut model = make_model_with_lsu(1, 1);
        let now = module_now(&scheduler);
        let request0 = GmemRequest::new(0, 16, 0xF, true);
        model
            .issue_gmem_request(now, 0, request0, &mut scheduler)
            .expect("first request should accept");

        let mut request1 = GmemRequest::new(0, 16, 0xF, true);
        request1.addr = 128;
        let retry_at = model
            .issue_gmem_request(now, 0, request1, &mut scheduler)
            .expect_err("second request should stall on LSU queue");
        assert!(retry_at > now);
        assert!(scheduler.get_schedule(0).is_none());
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

        let ready_at = ticket.ready_at();
        let mut cycle = now;
        let mut completed = false;
        for _ in 0..200 {
            model.tick(cycle, &mut scheduler);
            if model.stats().smem.completed >= 1 {
                completed = true;
                break;
            }
            cycle = cycle.saturating_add(1);
        }

        assert!(
            completed,
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
            serial: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            crossbar: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            subbank: ServerConfig {
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            },
            num_banks: 2,
            num_lanes: 1,
            num_subbanks: 1,
            word_bytes: 4,
            serialize_cores: false,
            link_capacity: 1,
        };
        let cfg = CoreGraphConfig {
            gmem,
            smem,
            ..CoreGraphConfig::default()
        };
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
        model
            .issue_smem_request(now, 1, req1, &mut scheduler)
            .expect("second SMEM request should be accepted into LSU");

        for cycle in now..now + 50 {
            model.tick(cycle, &mut scheduler);
        }

        let stats = model.stats().smem;
        assert!(
            stats.queue_full_rejects + stats.busy_rejects >= 1,
            "expected at least one rejection to be counted (queue_full={}, busy={})",
            stats.queue_full_rejects,
            stats.busy_rejects
        );
    }
}
