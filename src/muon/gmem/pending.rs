use std::collections::VecDeque;

use crate::muon::scheduler::Scheduler;
use crate::timeflow::{
    FenceRequest, GmemReject, SmemIssue, SmemReject, WritebackPayload,
};
use crate::timeflow::lsu::LsuPayload;
use crate::timeq::Cycle;

use super::{CoreTimingModel, PendingClusterIssue};

impl CoreTimingModel {
    pub(super) fn drive_lsu_issues(&mut self, now: Cycle) {
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

    pub(super) fn issue_pending_cluster_gmem(&mut self, now: Cycle) {
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
                Err(GmemReject { payload: request, retry_at, .. }) => {
                    pending.push_back(PendingClusterIssue {
                        request,
                        retry_at: retry_at.max(now.saturating_add(1)),
                    });
                }
            }
        }

        self.pending_cluster_gmem = pending;
    }

    pub(super) fn issue_pending_cluster_smem(&mut self, now: Cycle) {
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
                Err(SmemReject { payload: request, retry_at, .. }) => {
                    pending.push_back(PendingClusterIssue {
                        request,
                        retry_at: retry_at.max(now.saturating_add(1)),
                    });
                }
            }
        }

        self.pending_cluster_smem = pending;
    }

    pub(super) fn drain_pending_writeback(&mut self, now: Cycle) {
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

    pub(super) fn enqueue_writeback(&mut self, now: Cycle, payload: WritebackPayload) {
        if self.writeback.try_issue(now, payload.clone()).is_err() {
            self.pending_writeback.push_back(payload);
        }
    }

    pub(super) fn drain_pending_fence(&mut self, now: Cycle) {
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

    pub(super) fn enqueue_fence(&mut self, now: Cycle, request: FenceRequest) {
        if self.fence.try_issue(now, request.clone()).is_err() {
            self.pending_fence.push_back(request);
        }
    }

    pub(super) fn drain_pending_dma(&mut self, now: Cycle) {
        let mut remaining = VecDeque::new();
        while let Some(bytes) = self.pending_dma.pop_front() {
            if self.dma.try_issue(now, bytes).is_err() {
                remaining.push_back(bytes);
                remaining.extend(self.pending_dma.drain(..));
                break;
            }
        }
        self.pending_dma = remaining;
    }

    pub(super) fn enqueue_dma(&mut self, now: Cycle, bytes: u32) {
        if self.dma.try_issue(now, bytes).is_err() {
            self.pending_dma.push_back(bytes);
        }
    }

    pub(super) fn drain_pending_tensor(&mut self, now: Cycle) {
        let mut remaining = VecDeque::new();
        while let Some(bytes) = self.pending_tensor.pop_front() {
            if self.tensor.try_issue(now, bytes).is_err() {
                remaining.push_back(bytes);
                remaining.extend(self.pending_tensor.drain(..));
                break;
            }
        }
        self.pending_tensor = remaining;
    }

    pub(super) fn enqueue_tensor(&mut self, now: Cycle, bytes: u32) {
        if self.tensor.try_issue(now, bytes).is_err() {
            self.pending_tensor.push_back(bytes);
        }
    }

    pub(super) fn add_gmem_pending(
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

    pub(super) fn register_fence(&mut self, warp: usize, request_id: u64, scheduler: &mut Scheduler) {
        if !self.fence.is_enabled() {
            return;
        }
        if let Some(slot) = self.fence_inflight.get_mut(warp) {
            *slot = Some(request_id);
        }
        scheduler.set_resource_wait_until(warp, Some(Cycle::MAX));
    }

    pub(super) fn remove_gmem_pending(
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

    pub(super) fn add_smem_pending(
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

    pub(super) fn remove_smem_pending(
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
        let barrier_pending = self
            .barrier_inflight
            .get(warp)
            .copied()
            .unwrap_or(false);
        if !gmem_pending
            && !smem_pending
            && !icache_pending
            && !fence_pending
            && !barrier_pending
        {
            scheduler.clear_resource_wait(warp);
        }
    }
}
