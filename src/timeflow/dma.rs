use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
pub use crate::timeflow::types::RejectReason as DmaRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

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
    bytes_issued: u64,
    bytes_completed: u64,
}

impl DmaQueue {
    pub fn new(config: DmaConfig) -> Self {
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

    pub fn try_issue(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, DmaReject> {
        let res = self.queue.try_issue(now, (), bytes);
        if res.is_ok() {
            self.bytes_issued = self.bytes_issued.saturating_add(bytes as u64);
        }
        match res {
            Ok(ticket) => Ok(ticket),
            Err(err) => Err(DmaReject::new(err.retry_at, err.reason)),
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

