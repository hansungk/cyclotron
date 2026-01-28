use serde::Deserialize;

use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub struct DmaIssue {
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct DmaReject {
    pub retry_at: Cycle,
    pub reason: DmaRejectReason,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DmaConfig {
    pub enabled: bool,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for DmaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
    enabled: bool,
    server: TimedServer<()>,
    completed: u64,
}

impl DmaQueue {
    pub fn new(config: DmaConfig) -> Self {
        Self {
            enabled: config.enabled,
            server: TimedServer::new(config.queue),
            completed: 0,
        }
    }

    pub fn try_issue(&mut self, now: Cycle, bytes: u32) -> Result<DmaIssue, DmaReject> {
        if !self.enabled {
            return Ok(DmaIssue {
                ticket: Ticket::synthetic(now, now, bytes),
            });
        }
        let request = ServiceRequest::new((), bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(DmaIssue { ticket }),
            Err(Backpressure::Busy { available_at, .. }) => Err(DmaReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: DmaRejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(DmaReject {
                    retry_at,
                    reason: DmaRejectReason::QueueFull,
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
}
