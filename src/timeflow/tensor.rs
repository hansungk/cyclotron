use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
pub use crate::timeflow::types::RejectReason as TensorRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

// TensorIssue represented directly by `Ticket` to reduce wrapper boilerplate.

// Alias to central RejectReason for Tensor.

pub type TensorReject = crate::timeflow::types::Reject;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TensorConfig {
    pub enabled: bool,
    pub mmio_base: u64,
    pub mmio_size: u64,
    pub csr_addrs: Vec<u32>,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for TensorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mmio_base: 0,
            mmio_size: 0,
            csr_addrs: Vec::new(),
            queue: ServerConfig {
                base_latency: 20,
                bytes_per_cycle: 256,
                queue_capacity: 8,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
        }
    }
}

pub struct TensorQueue {
    queue: SimpleTimedQueue<()>,
    completed: u64,
    mmio_base: u64,
    mmio_size: u64,
    csr_addrs: Vec<u32>,
    bytes_issued: u64,
    bytes_completed: u64,
}

impl TensorQueue {
    pub fn new(config: TensorConfig) -> Self {
        Self {
            queue: SimpleTimedQueue::new(config.enabled, config.queue),
            completed: 0,
            mmio_base: config.mmio_base,
            mmio_size: config.mmio_size,
            csr_addrs: config.csr_addrs,
            bytes_issued: 0,
            bytes_completed: 0,
        }
    }

    pub fn try_issue(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, TensorReject> {
        let res = self.queue.try_issue(now, (), bytes);
        if res.is_ok() {
            self.bytes_issued = self.bytes_issued.saturating_add(bytes as u64);
        }
        match res {
            Ok(ticket) => Ok(ticket),
            Err(err) => Err(TensorReject::new(err.retry_at, err.reason)),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.queue.tick_with_service_result(now, |result| {
            self.completed = self.completed.saturating_add(1);
            self.bytes_completed = self
                .bytes_completed
                .saturating_add(result.ticket.size_bytes() as u64);
        });
    }

    pub fn bytes_issued(&self) -> u64 {
        self.bytes_issued
    }

    pub fn bytes_completed(&self) -> u64 {
        self.bytes_completed
    }

    pub fn is_busy(&self) -> bool {
        self.queue.is_busy()
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
    use super::{TensorConfig, TensorQueue};

    #[test]
    fn tensor_queue_completes() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 2;
        let mut tensor = TensorQueue::new(cfg);

        let ticket = tensor.try_issue(0, 256).expect("issue");
        tensor.tick(ticket.ready_at().saturating_sub(1));
        assert_eq!(tensor.completed(), 0);
        tensor.tick(ticket.ready_at());
        assert_eq!(tensor.completed(), 1);
    }

    #[test]
    fn disabled_tensor_immediate() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = false;
        let mut tensor = TensorQueue::new(cfg);
        let ticket = tensor.try_issue(5, 128).expect("issue");
        assert_eq!(ticket.ready_at(), 5);
    }

    #[test]
    fn queue_full_rejects() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = true;
        cfg.queue.queue_capacity = 1;
        let mut tensor = TensorQueue::new(cfg);
        assert!(tensor.try_issue(0, 64).is_ok());
        let err = tensor.try_issue(0, 64).expect_err("queue full");
        assert_eq!(super::TensorRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn zero_byte_operation_uses_base_latency() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 4;
        cfg.queue.bytes_per_cycle = 8;
        let mut tensor = TensorQueue::new(cfg);
        let ticket = tensor.try_issue(10, 0).unwrap();
        assert_eq!(14, ticket.ready_at());
    }

    #[test]
    fn tensor_large_operation_bandwidth_limited() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 3;
        cfg.queue.bytes_per_cycle = 8;
        let mut tensor = TensorQueue::new(cfg);

        let ticket = tensor.try_issue(0, 64).unwrap();
        assert_eq!(11, ticket.ready_at());
    }
}
