use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
use crate::timeflow::types::RejectReason;
pub use crate::timeflow::types::RejectReason as DmaRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

#[derive(Debug, Clone)]
pub struct DmaIssue {
    pub ticket: Ticket,
}

// Alias DmaRejectReason to the central RejectReason.

pub type DmaReject = crate::timeflow::types::Reject;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DmaConfig {
    pub enabled: bool,
    pub mmio_base: u64,
    pub mmio_size: u64,
    pub csr_addrs: Vec<u32>,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for DmaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mmio_base: 0,
            mmio_size: 0,
            csr_addrs: Vec::new(),
            queue: ServerConfig {
                base_latency: 10,
                bytes_per_cycle: 256,
                queue_capacity: 8,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
        }
    }
}

pub struct DmaQueue {
    queue: SimpleTimedQueue<()>,
    completed: u64,
    mmio_base: u64,
    mmio_size: u64,
    csr_addrs: Vec<u32>,
}

impl DmaQueue {
    pub fn new(config: DmaConfig) -> Self {
        Self {
            queue: SimpleTimedQueue::new(config.enabled, config.queue),
            completed: 0,
            mmio_base: config.mmio_base,
            mmio_size: config.mmio_size,
            csr_addrs: config.csr_addrs,
        }
    }

    pub fn try_issue(&mut self, now: Cycle, bytes: u32) -> Result<DmaIssue, DmaReject> {
        match self.queue.try_issue(now, (), bytes) {
            Ok(ticket) => Ok(DmaIssue { ticket }),
            Err(err) => Err(DmaReject { retry_at: err.retry_at, reason: err.reason }),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick(now, |_| {
            self.completed = self.completed.saturating_add(1);
        });
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn matches_mmio(&self, addr: u64) -> bool {
        if !self.queue.is_enabled() || self.mmio_size == 0 {
            return false;
        }
        let end = self.mmio_base.saturating_add(self.mmio_size);
        addr >= self.mmio_base && addr < end
    }

    pub fn matches_csr(&self, addr: u32) -> bool {
        if !self.queue.is_enabled() {
            return false;
        }
        self.csr_addrs.iter().any(|&csr| csr == addr)
    }
}

#[cfg(test)]
mod tests {
    use super::{DmaConfig, DmaQueue};

    #[test]
    fn dma_queue_completes() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 1;
        let mut dma = DmaQueue::new(cfg);

        let ticket = dma.try_issue(0, 128).expect("issue").ticket;
        dma.tick(ticket.ready_at().saturating_sub(1));
        assert_eq!(dma.completed(), 0);
        dma.tick(ticket.ready_at());
        assert_eq!(dma.completed(), 1);
    }

    #[test]
    fn disabled_dma_immediate() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = false;
        let mut dma = DmaQueue::new(cfg);
        let ticket = dma.try_issue(5, 64).expect("issue").ticket;
        assert_eq!(ticket.ready_at(), 5);
    }

    #[test]
    fn queue_full_rejects() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        let mut dma = DmaQueue::new(cfg);
        assert!(dma.try_issue(0, 64).is_ok());
        let err = dma.try_issue(0, 64).expect_err("queue full");
        assert_eq!(super::DmaRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn zero_byte_transfer_uses_base_latency() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 3;
        cfg.queue.bytes_per_cycle = 4;
        let mut dma = DmaQueue::new(cfg);
        let ticket = dma.try_issue(10, 0).unwrap().ticket;
        assert_eq!(13, ticket.ready_at());
    }

    #[test]
    fn dma_large_transfer_bandwidth_limited() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 2;
        cfg.queue.bytes_per_cycle = 8;
        let mut dma = DmaQueue::new(cfg);

        let ticket = dma.try_issue(0, 64).unwrap().ticket;
        assert_eq!(10, ticket.ready_at());
    }

    #[test]
    fn dma_very_large_transfer_no_overflow() {
        let mut cfg = DmaConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 1;
        cfg.queue.bytes_per_cycle = 1;
        let mut dma = DmaQueue::new(cfg);

        let ticket = dma.try_issue(0, u32::MAX).unwrap().ticket;
        assert_eq!(u32::MAX as u64 + 1, ticket.ready_at());
    }
}
