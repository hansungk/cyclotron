use std::collections::VecDeque;

use serde::Deserialize;

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

// Alias to central RejectReason for consistency across timeflow modules.

pub type WritebackReject = crate::timeflow::types::Reject;

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
}

impl WritebackQueue {
    pub fn new(config: WritebackConfig) -> Self {
        let mut cfg = config.queue;
        cfg.base_latency = cfg.base_latency.max(0);
        Self {
            queue: SimpleTimedQueue::new(config.enabled, cfg),
            ready: VecDeque::new(),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        payload: WritebackPayload,
    ) -> Result<WritebackIssue, WritebackReject> {
        if !self.queue.is_enabled() {
            self.ready.push_back(payload);
            return Ok(WritebackIssue {
                ticket: Ticket::synthetic(now, now, 0),
            });
        }

        match self.queue.try_issue(now, payload, 0) {
            Ok(ticket) => Ok(WritebackIssue { ticket }),
            Err(err) => Err(WritebackReject::new(err.retry_at, err.reason)),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick(now, |payload| {
            self.ready.push_back(payload);
        });
    }

    pub fn pop_ready(&mut self) -> Option<WritebackPayload> {
        self.ready.pop_front()
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    pub fn is_enabled(&self) -> bool {
        self.queue.is_enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::{WritebackConfig, WritebackPayload, WritebackQueue};
    use crate::timeflow::gmem::GmemCompletion;
    use crate::timeflow::gmem::GmemRequest;
    use crate::timeflow::smem::{SmemCompletion, SmemRequest};

    #[test]
    fn writeback_queue_throttles_completions() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 1;
        cfg.queue.completions_per_cycle = 1;
        cfg.queue.queue_capacity = 2;

        let mut queue = WritebackQueue::new(cfg);
        let req = GmemRequest::new(0, 4, 0xF, true);
        let completion = GmemCompletion {
            request: req.clone(),
            ticket_ready_at: 0,
            completed_at: 0,
        };
        let completion2 = GmemCompletion {
            request: req,
            ticket_ready_at: 0,
            completed_at: 0,
        };

        assert!(queue
            .try_issue(0, WritebackPayload::Gmem(completion))
            .is_ok());
        assert!(queue
            .try_issue(0, WritebackPayload::Gmem(completion2))
            .is_ok());

        queue.tick(0);
        assert!(queue.pop_ready().is_none());

        queue.tick(1);
        assert!(queue.pop_ready().is_some());
        assert!(queue.pop_ready().is_none());

        queue.tick(2);
        assert!(queue.pop_ready().is_some());
    }

    #[test]
    fn disabled_writeback_passthrough() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = false;
        let mut queue = WritebackQueue::new(cfg);
        let req = GmemRequest::new(0, 4, 0xF, true);
        let completion = GmemCompletion {
            request: req,
            ticket_ready_at: 0,
            completed_at: 0,
        };
        queue
            .try_issue(5, WritebackPayload::Gmem(completion))
            .unwrap();
        assert!(queue.pop_ready().is_some());
    }

    #[test]
    fn writeback_queue_handles_smem_payload() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 0;
        let mut queue = WritebackQueue::new(cfg);
        let req = SmemRequest::new(0, 4, 0xF, false, 0);
        let completion = SmemCompletion {
            request: req,
            ticket_ready_at: 0,
            completed_at: 0,
        };
        queue
            .try_issue(0, WritebackPayload::Smem(completion))
            .unwrap();
        queue.tick(0);
        assert!(matches!(queue.pop_ready(), Some(WritebackPayload::Smem(_))));
    }

    #[test]
    fn queue_full_rejects() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        let mut queue = WritebackQueue::new(cfg);
        let req = GmemRequest::new(0, 4, 0xF, true);
        let completion = GmemCompletion {
            request: req.clone(),
            ticket_ready_at: 0,
            completed_at: 0,
        };
        let completion2 = GmemCompletion {
            request: req,
            ticket_ready_at: 0,
            completed_at: 0,
        };
        queue
            .try_issue(0, WritebackPayload::Gmem(completion))
            .unwrap();
        let err = queue
            .try_issue(0, WritebackPayload::Gmem(completion2))
            .expect_err("queue should be full");
        assert_eq!(super::WritebackRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = true;
        let mut queue = WritebackQueue::new(cfg);
        assert!(queue.pop_ready().is_none());
    }

    #[test]
    fn interleaved_gmem_smem_completions() {
        let mut cfg = WritebackConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 0;
        let mut queue = WritebackQueue::new(cfg);

        let gmem = GmemCompletion {
            request: GmemRequest::new(0, 4, 0xF, true),
            ticket_ready_at: 0,
            completed_at: 0,
        };
        let smem = SmemCompletion {
            request: SmemRequest::new(0, 4, 0xF, false, 0),
            ticket_ready_at: 0,
            completed_at: 0,
        };

        queue.try_issue(0, WritebackPayload::Gmem(gmem)).unwrap();
        queue.try_issue(0, WritebackPayload::Smem(smem)).unwrap();
        queue.tick(0);

        let first = queue.pop_ready().expect("first");
        queue.tick(1);
        let second = queue.pop_ready().expect("second");
        assert!(matches!(first, WritebackPayload::Gmem(_)));
        assert!(matches!(second, WritebackPayload::Smem(_)));
    }
}
