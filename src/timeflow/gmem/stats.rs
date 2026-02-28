use crate::timeq::Cycle;
use serde::Serialize;
use std::ops::AddAssign;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct GmemStats {
    issued: u64,
    completed: u64,
    queue_full_rejects: u64,
    busy_rejects: u64,
    bytes_issued: u64,
    bytes_completed: u64,
    accesses: u64,
    hits: u64,
    bytes_hits: u64,
    inflight: u64,
    max_inflight: u64,
    max_completion_queue: u64,
    last_completion_cycle: Option<Cycle>,
}

impl GmemStats {
    pub fn issued(&self) -> u64 {
        self.issued
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn queue_full_rejects(&self) -> u64 {
        self.queue_full_rejects
    }

    pub fn busy_rejects(&self) -> u64 {
        self.busy_rejects
    }

    pub fn bytes_issued(&self) -> u64 {
        self.bytes_issued
    }

    pub fn bytes_completed(&self) -> u64 {
        self.bytes_completed
    }

    pub fn inflight(&self) -> u64 {
        self.inflight
    }

    pub fn accesses(&self) -> u64 {
        self.accesses
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn bytes_hits(&self) -> u64 {
        self.bytes_hits
    }

    pub fn max_inflight(&self) -> u64 {
        self.max_inflight
    }

    pub fn max_completion_queue(&self) -> u64 {
        self.max_completion_queue
    }

    pub fn last_completion_cycle(&self) -> Option<Cycle> {
        self.last_completion_cycle
    }

    pub fn record_issue(&mut self, bytes: u32) {
        self.issued = self.issued.saturating_add(1);
        self.bytes_issued = self.bytes_issued.saturating_add(bytes as u64);
        self.inflight = self.inflight.saturating_add(1);
        self.max_inflight = self.max_inflight.max(self.inflight);
    }

    pub fn record_access(&mut self, _bytes: u32) {
        self.accesses = self.accesses.saturating_add(1);
    }

    pub fn record_hit(&mut self, bytes: u32) {
        self.hits = self.hits.saturating_add(1);
        self.bytes_hits = self.bytes_hits.saturating_add(bytes as u64);
    }

    pub fn record_busy_reject(&mut self) {
        self.busy_rejects = self.busy_rejects.saturating_add(1);
    }

    pub fn record_queue_full_reject(&mut self) {
        self.queue_full_rejects = self.queue_full_rejects.saturating_add(1);
    }

    pub fn record_completion(&mut self, bytes: u32, now: Cycle) {
        self.completed = self.completed.saturating_add(1);
        self.bytes_completed = self.bytes_completed.saturating_add(bytes as u64);
        self.inflight = self.inflight.saturating_sub(1);
        self.last_completion_cycle = Some(now);
    }

    pub fn update_completion_queue(&mut self, completion_len: usize) {
        self.max_completion_queue = self.max_completion_queue.max(completion_len as u64);
    }

    pub fn accumulate_from(&mut self, other: &GmemStats) {
        *self += other;
    }
}

impl AddAssign<&GmemStats> for GmemStats {
    fn add_assign(&mut self, other: &GmemStats) {
        self.issued = self.issued.saturating_add(other.issued);
        self.completed = self.completed.saturating_add(other.completed);
        self.accesses = self.accesses.saturating_add(other.accesses);
        self.hits = self.hits.saturating_add(other.hits);
        self.bytes_hits = self.bytes_hits.saturating_add(other.bytes_hits);
        self.queue_full_rejects = self
            .queue_full_rejects
            .saturating_add(other.queue_full_rejects);
        self.busy_rejects = self.busy_rejects.saturating_add(other.busy_rejects);
        self.bytes_issued = self.bytes_issued.saturating_add(other.bytes_issued);
        self.bytes_completed = self.bytes_completed.saturating_add(other.bytes_completed);
        self.max_inflight = self.max_inflight.max(other.max_inflight);
        self.max_completion_queue = self.max_completion_queue.max(other.max_completion_queue);
        self.last_completion_cycle = match (self.last_completion_cycle, other.last_completion_cycle)
        {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, Some(b)) => Some(b),
            (a, None) => a,
        };
    }
}

impl AddAssign<GmemStats> for GmemStats {
    fn add_assign(&mut self, other: GmemStats) {
        *self += &other;
    }
}
