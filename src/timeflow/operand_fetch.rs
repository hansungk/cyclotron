use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
use crate::timeflow::types::RejectReason;
pub use crate::timeflow::types::RejectReason as OperandFetchRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

#[derive(Debug, Clone)]
pub struct OperandFetchIssue {
    pub ticket: Ticket,
}

// Alias to central RejectReason for OperandFetch.

pub type OperandFetchReject = crate::timeflow::types::Reject;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OperandFetchConfig {
    pub enabled: bool,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for OperandFetchConfig {
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

pub struct OperandFetchQueue {
    queue: SimpleTimedQueue<()>,
}

impl OperandFetchQueue {
    pub fn new(config: OperandFetchConfig) -> Self {
        Self {
            queue: SimpleTimedQueue::new(config.enabled, config.queue),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        bytes: u32,
    ) -> Result<OperandFetchIssue, OperandFetchReject> {
        match self.queue.try_issue(now, (), bytes) {
            Ok(ticket) => Ok(OperandFetchIssue { ticket }),
            Err(err) => Err(OperandFetchReject { retry_at: err.retry_at, reason: err.reason }),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick(now, |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::{OperandFetchConfig, OperandFetchQueue};

    #[test]
    fn operand_fetch_queue_limits_admission() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        cfg.queue.base_latency = 2;
        let mut queue = OperandFetchQueue::new(cfg);

        let ticket = queue.try_issue(0, 16).expect("first issue").ticket;
        assert!(queue.try_issue(0, 16).is_err());

        let ready_at = ticket.ready_at();
        queue.tick(ready_at.saturating_sub(1));
        assert!(queue.try_issue(ready_at.saturating_sub(1), 16).is_err());
        queue.tick(ready_at);
        assert!(queue.try_issue(ready_at, 16).is_ok());
    }

    #[test]
    fn disabled_operand_fetch_immediate() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = false;
        let mut queue = OperandFetchQueue::new(cfg);
        let ticket = queue.try_issue(5, 16).expect("issue").ticket;
        assert_eq!(ticket.ready_at(), 5);
    }

    #[test]
    fn zero_byte_fetch_uses_base_latency() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 3;
        cfg.queue.bytes_per_cycle = 4;
        let mut queue = OperandFetchQueue::new(cfg);
        let ticket = queue.try_issue(10, 0).expect("issue").ticket;
        assert_eq!(13, ticket.ready_at());
    }

    #[test]
    fn multiple_operands_queue_fifo() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 2;
        cfg.queue.base_latency = 0;
        let mut queue = OperandFetchQueue::new(cfg);

        let t0 = queue.try_issue(0, 4).expect("issue0").ticket;
        let t1 = queue.try_issue(0, 4).expect("issue1").ticket;
        assert!(t1.ready_at() >= t0.ready_at());

        queue.tick(t0.ready_at());
        let t2 = queue.try_issue(t0.ready_at(), 4).expect("issue2").ticket;
        assert!(t2.ready_at() >= t0.ready_at());
    }
}
