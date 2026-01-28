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
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for TensorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
}

impl TensorQueue {
    pub fn new(config: TensorConfig) -> Self {
        Self {
            enabled: config.enabled,
            server: TimedServer::new(config.queue),
            completed: 0,
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
}
