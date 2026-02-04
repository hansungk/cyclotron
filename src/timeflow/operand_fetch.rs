use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
pub use crate::timeflow::types::RejectReason as OperandFetchRejectReason;
use crate::timeq::ServerConfig;

// Alias to the central Reject type used by queues.
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

// Collapse the thin wrapper: expose the underlying SimpleTimedQueue<()> directly.
pub type OperandFetchQueue = SimpleTimedQueue<()>;

#[cfg(test)]
mod tests {
    use super::{OperandFetchConfig, OperandFetchQueue};
    use crate::timeq::Ticket;

    #[test]
    fn operand_fetch_queue_limits_admission() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        cfg.queue.base_latency = 2;
        let mut queue = OperandFetchQueue::new(cfg.enabled, cfg.queue);

        let ticket: Ticket = queue.try_issue(0, (), 16).expect("first issue");
        assert!(queue.try_issue(0, (), 16).is_err());

        let ready_at = ticket.ready_at();
        queue.tick(ready_at.saturating_sub(1), |_| {});
        assert!(queue.try_issue(ready_at.saturating_sub(1), (), 16).is_err());
        queue.tick(ready_at, |_| {});
        assert!(queue.try_issue(ready_at, (), 16).is_ok());
    }

    #[test]
    fn disabled_operand_fetch_immediate() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = false;
        let mut queue = OperandFetchQueue::new(cfg.enabled, cfg.queue);
        let ticket: Ticket = queue.try_issue(5, (), 16).expect("issue");
        assert_eq!(ticket.ready_at(), 5);
    }

    #[test]
    fn zero_byte_fetch_uses_base_latency() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 3;
        cfg.queue.bytes_per_cycle = 4;
        let mut queue = OperandFetchQueue::new(cfg.enabled, cfg.queue);
        let ticket: Ticket = queue.try_issue(10, (), 0).expect("issue");
        assert_eq!(13, ticket.ready_at());
    }

    #[test]
    fn multiple_operands_queue_fifo() {
        let mut cfg = OperandFetchConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 2;
        cfg.queue.base_latency = 0;
        let mut queue = OperandFetchQueue::new(cfg.enabled, cfg.queue);

        let t0: Ticket = queue.try_issue(0, (), 4).expect("issue0");
        let t1: Ticket = queue.try_issue(0, (), 4).expect("issue1");
        assert!(t1.ready_at() >= t0.ready_at());

        queue.tick(t0.ready_at(), |_| {});
        let t2: Ticket = queue.try_issue(t0.ready_at(), (), 4).expect("issue2");
        assert!(t2.ready_at() >= t0.ready_at());
    }
}
