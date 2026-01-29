use serde::Deserialize;

use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub struct TensorIssue {
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct TensorReject {
    pub retry_at: Cycle,
    pub reason: TensorRejectReason,
}

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
    enabled: bool,
    server: TimedServer<()>,
    completed: u64,
    mmio_base: u64,
    mmio_size: u64,
    csr_addrs: Vec<u32>,
}

impl TensorQueue {
    pub fn new(config: TensorConfig) -> Self {
        Self {
            enabled: config.enabled,
            server: TimedServer::new(config.queue),
            completed: 0,
            mmio_base: config.mmio_base,
            mmio_size: config.mmio_size,
            csr_addrs: config.csr_addrs,
        }
    }

    pub fn try_issue(&mut self, now: Cycle, bytes: u32) -> Result<TensorIssue, TensorReject> {
        if !self.enabled {
            return Ok(TensorIssue {
                ticket: Ticket::synthetic(now, now, bytes),
            });
        }
        let request = ServiceRequest::new((), bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(TensorIssue { ticket }),
            Err(Backpressure::Busy { available_at, .. }) => Err(TensorReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: TensorRejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(TensorReject {
                    retry_at,
                    reason: TensorRejectReason::QueueFull,
                })
            }
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        if !self.enabled {
            return;
        }
        self.server.service_ready(now, |_| {
            self.completed = self.completed.saturating_add(1);
        });
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn matches_mmio(&self, addr: u64) -> bool {
        if !self.enabled || self.mmio_size == 0 {
            return false;
        }
        let end = self.mmio_base.saturating_add(self.mmio_size);
        addr >= self.mmio_base && addr < end
    }

    pub fn matches_csr(&self, addr: u32) -> bool {
        if !self.enabled {
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

        let ticket = tensor.try_issue(0, 256).expect("issue").ticket;
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
        let ticket = tensor.try_issue(5, 128).expect("issue").ticket;
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
        let ticket = tensor.try_issue(10, 0).unwrap().ticket;
        assert_eq!(14, ticket.ready_at());
    }

    #[test]
    fn tensor_large_operation_bandwidth_limited() {
        let mut cfg = TensorConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 3;
        cfg.queue.bytes_per_cycle = 8;
        let mut tensor = TensorQueue::new(cfg);

        let ticket = tensor.try_issue(0, 64).unwrap().ticket;
        assert_eq!(11, ticket.ready_at());
    }
}
