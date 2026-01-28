use crate::info;
use crate::muon::scheduler::Scheduler;
use crate::timeflow::lsu::LsuPayload;
use crate::timeq::Cycle;

use super::{CoreTimingModel, SmemConflictSample};

impl CoreTimingModel {
    fn record_gmem_completion(
        &mut self,
        now: Cycle,
        completion: &crate::timeflow::GmemCompletion,
    ) {
        if completion.request.kind.is_mem() {
            self.gmem_hits.l0_accesses = self.gmem_hits.l0_accesses.saturating_add(1);
            if completion.request.l0_hit {
                self.gmem_hits.l0_hits = self.gmem_hits.l0_hits.saturating_add(1);
            } else {
                self.gmem_hits.l1_accesses = self.gmem_hits.l1_accesses.saturating_add(1);
                if completion.request.l1_hit {
                    self.gmem_hits.l1_hits = self.gmem_hits.l1_hits.saturating_add(1);
                } else {
                    self.gmem_hits.l2_accesses =
                        self.gmem_hits.l2_accesses.saturating_add(1);
                    if completion.request.l2_hit {
                        self.gmem_hits.l2_hits = self.gmem_hits.l2_hits.saturating_add(1);
                    }
                }
            }
        }

        if let Some(issue_at) = self.gmem_issue_cycle.get(&completion.request.id).copied() {
            let latency = now.saturating_sub(issue_at);
            self.latencies.gmem_count = self.latencies.gmem_count.saturating_add(1);
            self.latencies.gmem_sum = self.latencies.gmem_sum.saturating_add(latency);
            if let Some(log) = self.mem_latency.as_mut() {
                log.write_gmem(
                    now,
                    self.core_id,
                    completion.request.warp,
                    completion.request.id,
                    completion.request.bytes,
                    issue_at,
                    latency,
                    completion.request.l0_hit,
                    completion.request.l1_hit,
                    completion.request.l2_hit,
                );
            }
        }
    }

    fn record_smem_completion(
        &mut self,
        now: Cycle,
        completion: &crate::timeflow::SmemCompletion,
    ) {
        if let Some(issue_at) = self.smem_issue_cycle.get(&completion.request.id).copied() {
            let latency = now.saturating_sub(issue_at);
            self.latencies.smem_count = self.latencies.smem_count.saturating_add(1);
            self.latencies.smem_sum = self.latencies.smem_sum.saturating_add(latency);
            if let Some(log) = self.mem_latency.as_mut() {
                log.write_smem(
                    now,
                    self.core_id,
                    completion.request.warp,
                    completion.request.id,
                    completion.request.bytes,
                    issue_at,
                    latency,
                );
            }
        }
    }

    fn maybe_clear_gmem_issue_cycle(&mut self, request_id: u64) {
        if self
            .pending_gmem
            .iter()
            .any(|queue| queue.iter().any(|(id, _)| *id == request_id))
        {
            return;
        }
        self.gmem_issue_cycle.remove(&request_id);
    }

    fn maybe_clear_smem_issue_cycle(&mut self, request_id: u64) {
        if self
            .pending_smem
            .iter()
            .any(|queue| queue.iter().any(|(id, _)| *id == request_id))
        {
            return;
        }
        self.smem_issue_cycle.remove(&request_id);
    }

    pub(super) fn record_smem_conflict(
        &mut self,
        now: Cycle,
        warp: usize,
        request_id: u64,
        sample: SmemConflictSample,
    ) {
        self.smem_conflicts_summary.instructions =
            self.smem_conflicts_summary.instructions.saturating_add(1);
        self.smem_conflicts_summary.active_lanes = self
            .smem_conflicts_summary
            .active_lanes
            .saturating_add(sample.active_lanes as u64);
        self.smem_conflicts_summary.conflict_lanes = self
            .smem_conflicts_summary
            .conflict_lanes
            .saturating_add(sample.conflict_lanes as u64);
        self.smem_conflicts_summary.unique_banks = self
            .smem_conflicts_summary
            .unique_banks
            .saturating_add(sample.unique_banks as u64);
        self.smem_conflicts_summary.unique_subbanks = self
            .smem_conflicts_summary
            .unique_subbanks
            .saturating_add(sample.unique_subbanks as u64);

        if let Some(log) = self.smem_conflicts.as_mut() {
            log.write_row(
                now,
                self.core_id,
                warp,
                request_id,
                sample.active_lanes,
                sample.unique_banks,
                sample.unique_subbanks,
                sample.conflict_lanes,
            );
        }
    }

    pub(super) fn handle_gmem_completion(
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
        self.record_gmem_completion(now, &completion);
        self.maybe_clear_gmem_issue_cycle(completed_id);
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

    pub(super) fn handle_smem_completion(
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
        self.record_smem_completion(now, &completion);
        self.maybe_clear_smem_issue_cycle(completed_id);
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
}
