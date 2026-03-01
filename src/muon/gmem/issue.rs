use crate::info;
use crate::muon::decode::IssuedInst;
use crate::muon::execute::Opcode;
use crate::muon::scheduler::Scheduler;
use crate::timeflow::lsu::LsuPayload;
use crate::timeflow::{
    execute::ExecUnitKind, GmemRequest, GmemRequestKind, IcacheIssue, IcacheReject, IcacheRequest,
    LsuIssue, LsuReject, LsuRejectReason, SmemRequest,
};
use crate::timeq::{normalize_retry, Backpressure, Cycle, Ticket};

use super::{CoreTimingModel, IcacheInflight};

impl CoreTimingModel {
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
                // coalesce at the first cache level's line size.
                let line_bytes = if self.gmem_policy.l0_enabled {
                    self.gmem_policy.l0_line_bytes.max(1)
                } else {
                    self.gmem_policy.l1_line_bytes.max(1)
                } as u64;
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
        if request.stall_on_completion {
            if let Some(slot) = self.pending_gmem.get(warp) {
                if !slot.is_empty() {
                    let wait_until = slot
                        .iter()
                        .map(|(_, ready_at)| *ready_at)
                        .min()
                        .unwrap_or(now.saturating_add(1))
                        .max(now.saturating_add(1));
                    scheduler.set_resource_wait_until(warp, Some(wait_until));
                    scheduler.replay_instruction(warp);
                    return Err(wait_until);
                }
            }
        }
        let issue_bytes = request.bytes;
        let request_id = request.id;
        let dma_trigger = !request.is_load && self.graph.dma_matches_mmio(request.addr);
        let tensor_trigger = !request.is_load && self.graph.tensor_matches_mmio(request.addr);
        if request_id >= self.next_gmem_id {
            self.next_gmem_id = request_id.saturating_add(1);
        }
        let split_count = request
            .coalesced_lines
            .as_ref()
            .map(|lines| lines.len().max(1))
            .unwrap_or(1);
        let is_flush = request.kind.is_flush_l0() || request.kind.is_flush_l1();
        if let Err(reject) = self.graph.operand_fetch_try_issue(now, request.bytes) {
            let wait_until = reject.retry_at.max(now.saturating_add(1));
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            return Err(wait_until);
        }
        let issue_result = self.graph.lsu_issue_gmem(now, request);
        match issue_result {
            Ok(LsuIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                self.gmem_issue_cycle.entry(request_id).or_insert(now);
                self.add_gmem_pending(warp, request_id, ready_at, scheduler, split_count);
                if is_flush {
                    self.register_fence(warp, request_id, scheduler);
                }
                if dma_trigger {
                    self.enqueue_dma(now, issue_bytes.max(1));
                }
                if tensor_trigger {
                    self.enqueue_tensor(now, issue_bytes.max(1));
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
                payload: request,
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
            request.id = if self.next_smem_id == 0 {
                1
            } else {
                self.next_smem_id
            };
        } else if request.id >= self.next_smem_id {
            self.next_smem_id = request.id.saturating_add(1);
        }
        if request.id >= self.next_smem_id {
            self.next_smem_id = request.id.saturating_add(1);
        }
        let request_id = request.id;
        let split_count = self.split_smem_request(&request).len().max(1);
        let conflict_sample = self.compute_smem_conflict(&request);
        let issue_bytes = request.bytes;
        if let Err(reject) = self.graph.operand_fetch_try_issue(now, request.bytes) {
            let wait_until = reject.retry_at.max(now.saturating_add(1));
            scheduler.set_resource_wait_until(warp, Some(wait_until));
            scheduler.replay_instruction(warp);
            return Err(wait_until);
        }
        let issue_result = self.graph.lsu_issue_smem(now, request);
        match issue_result {
            Ok(LsuIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                self.smem_issue_cycle.entry(request_id).or_insert(now);
                self.add_smem_pending(warp, request_id, ready_at, scheduler, split_count);
                if let Some(sample) = conflict_sample {
                    self.record_smem_conflict(now, warp, request_id, sample);
                }
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
                payload: request,
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

    pub fn issue_smem_request_frontend(
        &mut self,
        now: Cycle,
        mut request: SmemRequest,
    ) -> Result<Ticket, Cycle> {
        let warp = request.warp;
        if warp >= self.pending_smem.len() {
            return Err(now.saturating_add(1));
        }

        if request.id == 0 {
            request.id = if self.next_smem_id == 0 {
                1
            } else {
                self.next_smem_id
            };
        } else if request.id >= self.next_smem_id {
            self.next_smem_id = request.id.saturating_add(1);
        }
        if request.id >= self.next_smem_id {
            self.next_smem_id = request.id.saturating_add(1);
        }
        let request_id = request.id;
        let split_count = self.split_smem_request(&request).len().max(1);
        let conflict_sample = self.compute_smem_conflict(&request);
        let issue_bytes = request.bytes;
        if let Err(reject) = self.graph.operand_fetch_try_issue(now, request.bytes) {
            let wait_until = reject.retry_at.max(now.saturating_add(1));
            return Err(wait_until);
        }

        match self.graph.lsu_issue_smem(now, request) {
            Ok(LsuIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                self.smem_issue_cycle.entry(request_id).or_insert(now);
                self.add_smem_pending_frontend(warp, request_id, ready_at, split_count);
                if let Some(sample) = conflict_sample {
                    self.record_smem_conflict(now, warp, request_id, sample);
                }
                self.trace_event(
                    now,
                    "smem_issue_frontend",
                    warp,
                    Some(request_id),
                    issue_bytes,
                    None,
                );
                Ok(ticket)
            }
            Err(LsuReject {
                payload: request,
                retry_at,
                reason,
            }) => {
                let wait_until = retry_at.max(now.saturating_add(1));
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
                    "smem_reject_frontend",
                    warp,
                    Some(request_id),
                    request_bytes,
                    Some(reason_str),
                );
                Err(wait_until)
            }
        }
    }

    pub fn issue_dma(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, Cycle> {
        match self.graph.dma_try_issue(now, bytes) {
            Ok(ticket) => Ok(ticket),
            Err(reject) => Err(reject.retry_at),
        }
    }

    pub fn issue_tensor(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, Cycle> {
        match self.graph.tensor_try_issue(now, bytes) {
            Ok(ticket) => Ok(ticket),
            Err(reject) => Err(reject.retry_at),
        }
    }

    pub fn issue_execute(
        &mut self,
        now: Cycle,
        warp: usize,
        issued: &IssuedInst,
        active_lanes: u32,
        scheduler: &mut Scheduler,
    ) -> Result<Ticket, Cycle> {
        if warp >= self.pending_execute.len() {
            return Ok(Ticket::new(now, now, active_lanes.max(1)));
        }
        if active_lanes == 0 {
            return Ok(Ticket::new(now, now, 0));
        }

        let kind = match exec_unit_for(issued) {
            Some(kind) => kind,
            None => return Ok(Ticket::new(now, now, active_lanes.max(1))),
        };

        if let Some(ready_at) = self.pending_execute[warp] {
            if now >= ready_at {
                self.pending_execute[warp] = None;
                self.trace_event(now, "exec_complete", warp, None, active_lanes, None);
                self.update_scheduler_state(warp, scheduler);
                return Ok(Ticket::new(now, now, active_lanes.max(1)));
            }
            scheduler.set_resource_wait_until(warp, Some(ready_at));
            scheduler.replay_instruction(warp);
            return Err(ready_at);
        }

        match self.graph.execute_issue(now, kind, active_lanes) {
            Ok(ticket) => {
                let ready_at = ticket.ready_at();
                if ready_at > now {
                    self.pending_execute[warp] = Some(ready_at);
                    scheduler.set_resource_wait_until(warp, Some(ready_at));
                    scheduler.replay_instruction(warp);
                    self.trace_event(now, "exec_issue", warp, None, active_lanes, None);
                    return Err(ready_at);
                }
                self.trace_event(now, "exec_issue", warp, None, active_lanes, None);
                Ok(ticket)
            }
            Err(bp) => {
                let (wait_until, reason_str) = match bp {
                    Backpressure::Busy { available_at, .. } => {
                        (normalize_retry(now, available_at), "busy")
                    }
                    Backpressure::QueueFull { .. } => (
                        normalize_retry(now, self.graph.execute_suggest_retry(kind)),
                        "queue_full",
                    ),
                };
                let wait_until = wait_until.max(now.saturating_add(1));
                scheduler.set_resource_wait_until(warp, Some(wait_until));
                scheduler.replay_instruction(warp);
                self.trace_event(
                    now,
                    "exec_reject",
                    warp,
                    None,
                    active_lanes,
                    Some(reason_str),
                );
                Err(wait_until)
            }
        }
    }

    pub fn notify_csr_write(&mut self, now: Cycle, csr_addr: u32) {
        if self.graph.dma_matches_csr(csr_addr) {
            self.enqueue_dma(now, 4);
        }
        if self.graph.tensor_matches_csr(csr_addr) {
            self.enqueue_tensor(now, 4);
        }
    }

    pub fn select_issue_mask(&mut self, now: Cycle, eligible: &[bool]) -> Vec<bool> {
        self.issue_scheduler.select(now, eligible)
    }

    pub fn record_issue_stats(
        &mut self,
        now: Cycle,
        active_warps: u32,
        eligible_warps: u32,
        issued_warps: u32,
    ) {
        if self.last_issue_stats_cycle == Some(now) {
            return;
        }
        self.last_issue_stats_cycle = Some(now);
        self.scheduler_stats.cycles = self.scheduler_stats.cycles.saturating_add(1);
        self.scheduler_stats.active_warps_sum = self
            .scheduler_stats
            .active_warps_sum
            .saturating_add(active_warps as u64);
        self.scheduler_stats.eligible_warps_sum = self
            .scheduler_stats
            .eligible_warps_sum
            .saturating_add(eligible_warps as u64);
        self.scheduler_stats.issued_warps_sum = self
            .scheduler_stats
            .issued_warps_sum
            .saturating_add(issued_warps as u64);
        if self.scheduler_stats.issue_width == 0 {
            self.scheduler_stats.issue_width = 1;
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

        match self.graph.issue_icache(now, request) {
            Ok(IcacheIssue { ticket }) => {
                let ready_at = ticket.ready_at();
                if ready_at <= now {
                    true
                } else {
                    self.icache_inflight[warp] = Some(IcacheInflight { ready_at });
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
}

fn exec_unit_for(inst: &IssuedInst) -> Option<ExecUnitKind> {
    if inst.opcode == Opcode::LOAD
        || inst.opcode == Opcode::STORE
        || inst.opcode == Opcode::MISC_MEM
        || inst.opcode == Opcode::LOAD_FP
        || inst.opcode == Opcode::STORE_FP
    {
        return None;
    }

    match inst.opcode {
        Opcode::SYSTEM => {
            if inst.f3 == 0 {
                Some(ExecUnitKind::Sfu)
            } else {
                Some(ExecUnitKind::Int)
            }
        }
        Opcode::CUSTOM0 => Some(ExecUnitKind::Sfu),
        Opcode::CUSTOM2 => Some(ExecUnitKind::Sfu),
        Opcode::OP_FP | Opcode::MADD | Opcode::MSUB | Opcode::NM_ADD | Opcode::NM_SUB => {
            Some(ExecUnitKind::Fp)
        }
        Opcode::OP => {
            if inst.f7 == 0b0000001 {
                match inst.f3 {
                    0b000 | 0b001 | 0b010 | 0b011 => Some(ExecUnitKind::IntMul),
                    0b100 | 0b101 | 0b110 | 0b111 => Some(ExecUnitKind::IntDiv),
                    _ => Some(ExecUnitKind::Int),
                }
            } else {
                Some(ExecUnitKind::Int)
            }
        }
        _ => Some(ExecUnitKind::Int),
    }
}
