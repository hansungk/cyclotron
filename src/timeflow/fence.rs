use std::collections::VecDeque;

use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
use crate::timeflow::types::RejectReason;
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

// Alias to central RejectReason for fences.

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
                ticket: Ticket::synthetic(now, now, 0),
            });
        }

        match self.queue.try_issue(now, request, 0) {
            Ok(ticket) => Ok(FenceIssue { ticket }),
            Err(err) => Err(FenceReject { retry_at: err.retry_at, reason: err.reason }),
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

#[cfg(test)]
mod tests {
    use super::{FenceConfig, FenceQueue, FenceRequest};

    #[test]
    fn fence_queue_delays_release() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 2;
        cfg.queue.queue_capacity = 1;
        let mut fence = FenceQueue::new(cfg);

        assert!(fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 1 })
            .is_ok());
        fence.tick(1);
        assert!(fence.pop_ready().is_none());
        fence.tick(2);
        assert!(fence.pop_ready().is_some());
    }

    #[test]
    fn disabled_fence_immediate() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = false;
        let mut fence = FenceQueue::new(cfg);
        fence
            .try_issue(5, FenceRequest { warp: 0, request_id: 1 })
            .unwrap();
        assert!(fence.pop_ready().is_some());
    }

    #[test]
    fn queue_full_rejects() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        let mut fence = FenceQueue::new(cfg);

        fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 1 })
            .unwrap();
        let err = fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 2 })
            .expect_err("queue should be full");
        assert_eq!(super::FenceRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn multiple_fences_queue_fifo() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 0;
        cfg.queue.queue_capacity = 4;
        let mut fence = FenceQueue::new(cfg);

        fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 1 })
            .unwrap();
        fence
            .try_issue(0, FenceRequest { warp: 1, request_id: 2 })
            .unwrap();

        fence.tick(0);
        let first = fence.pop_ready().expect("first ready");
        fence.tick(1);
        let second = fence.pop_ready().expect("second ready");
        assert_eq!(first.request_id, 1);
        assert_eq!(second.request_id, 2);
    }

    #[test]
    fn fence_at_cycle_zero() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 0;
        let mut fence = FenceQueue::new(cfg);

        fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 1 })
            .unwrap();
        fence.tick(0);
        assert!(fence.pop_ready().is_some());
    }
}
