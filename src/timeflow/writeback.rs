use std::collections::VecDeque;
use std::ops::AddAssign;

use serde::Deserialize;
use serde::Serialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
pub use crate::timeflow::types::RejectReason as WritebackRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

use super::gmem::GmemCompletion;
use super::smem::SmemCompletion;

#[derive(Debug, Clone)]
pub enum WritebackPayload {
    Gmem(GmemCompletion),
    Smem(SmemCompletion),
}

#[derive(Debug, Clone)]
pub struct WritebackIssue {
    pub ticket: Ticket,
}

pub type WritebackReject = crate::timeflow::types::Reject;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct WritebackStats {
    pub issued: u64,
    pub completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
}

impl AddAssign<&WritebackStats> for WritebackStats {
    fn add_assign(&mut self, other: &WritebackStats) {
        self.issued = self.issued.saturating_add(other.issued);
        self.completed = self.completed.saturating_add(other.completed);
        self.queue_full_rejects = self
            .queue_full_rejects
            .saturating_add(other.queue_full_rejects);
        self.busy_rejects = self.busy_rejects.saturating_add(other.busy_rejects);
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WritebackConfig {
    pub enabled: bool,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for WritebackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            queue: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 1024,
                queue_capacity: 16,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
        }
    }
}

pub struct WritebackQueue {
    queue: SimpleTimedQueue<WritebackPayload>,
    ready: VecDeque<WritebackPayload>,
    stats: WritebackStats,
}

impl WritebackQueue {
    pub fn new(config: WritebackConfig) -> Self {
        let mut cfg = config.queue;
        cfg.base_latency = cfg.base_latency.max(0);
        Self {
            queue: SimpleTimedQueue::new(config.enabled, cfg),
            ready: VecDeque::new(),
            stats: WritebackStats::default(),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        payload: WritebackPayload,
    ) -> Result<WritebackIssue, WritebackReject> {
        if !self.queue.is_enabled() {
            self.ready.push_back(payload);
            self.stats.issued = self.stats.issued.saturating_add(1);
            return Ok(WritebackIssue {
                ticket: Ticket::new(now, now, 0),
            });
        }

        match self.queue.try_issue(now, payload, 0) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                Ok(WritebackIssue { ticket })
            }
            Err(err) => {
                match err.reason {
                    WritebackRejectReason::QueueFull => {
                        self.stats.queue_full_rejects =
                            self.stats.queue_full_rejects.saturating_add(1);
                    }
                    WritebackRejectReason::Busy => {
                        self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                    }
                }
                Err(WritebackReject::new(err.retry_at, err.reason))
            }
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick(now, |payload| {
            self.ready.push_back(payload);
        });
    }

    pub fn pop_ready(&mut self) -> Option<WritebackPayload> {
        let popped = self.ready.pop_front();
        if popped.is_some() {
            self.stats.completed = self.stats.completed.saturating_add(1);
        }
        popped
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    pub fn is_enabled(&self) -> bool {
        self.queue.is_enabled()
    }

    pub fn stats(&self) -> WritebackStats {
        self.stats
    }

    pub fn clear_stats(&mut self) {
        self.stats = WritebackStats::default();
    }
}
