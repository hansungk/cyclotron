use std::collections::VecDeque;

use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
pub use crate::timeflow::types::RejectReason as FenceRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

#[derive(Debug, Clone)]
pub struct FenceRequest {
    pub warp: usize,
    pub request_id: u64,
}

#[derive(Debug, Clone)]
pub struct FenceIssue {
    pub ticket: Ticket,
}

pub type FenceReject = crate::timeflow::types::Reject;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FenceConfig {
    pub enabled: bool,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for FenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            queue: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 1,
                queue_capacity: 4,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
        }
    }
}

pub struct FenceQueue {
    queue: SimpleTimedQueue<FenceRequest>,
    ready: VecDeque<FenceRequest>,
}

impl FenceQueue {
    pub fn new(config: FenceConfig) -> Self {
        Self {
            queue: SimpleTimedQueue::new(config.enabled, config.queue),
            ready: VecDeque::new(),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        request: FenceRequest,
    ) -> Result<FenceIssue, FenceReject> {
        if !self.queue.is_enabled() {
            self.ready.push_back(request);
            return Ok(FenceIssue {
                ticket: Ticket::new(now, now, 0),
            });
        }

        match self.queue.try_issue(now, request, 0) {
            Ok(ticket) => Ok(FenceIssue { ticket }),
            Err(err) => Err(FenceReject::new(err.retry_at, err.reason)),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick(now, |payload| {
            self.ready.push_back(payload);
        });
    }

    pub fn pop_ready(&mut self) -> Option<FenceRequest> {
        self.ready.pop_front()
    }

    pub fn is_enabled(&self) -> bool {
        self.queue.is_enabled()
    }
}

