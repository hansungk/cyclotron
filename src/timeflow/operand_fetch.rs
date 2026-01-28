use serde::Deserialize;

use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub struct OperandFetchIssue {
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandFetchRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct OperandFetchReject {
    pub retry_at: Cycle,
    pub reason: OperandFetchRejectReason,
}

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
    enabled: bool,
    server: TimedServer<()>,
}

impl OperandFetchQueue {
    pub fn new(config: OperandFetchConfig) -> Self {
        Self {
            enabled: config.enabled,
            server: TimedServer::new(config.queue),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        bytes: u32,
    ) -> Result<OperandFetchIssue, OperandFetchReject> {
        if !self.enabled {
            return Ok(OperandFetchIssue {
                ticket: Ticket::synthetic(now, now, bytes),
            });
        }

        let request = ServiceRequest::new((), bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(OperandFetchIssue { ticket }),
            Err(Backpressure::Busy { available_at, .. }) => Err(OperandFetchReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: OperandFetchRejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(OperandFetchReject {
                    retry_at,
                    reason: OperandFetchRejectReason::QueueFull,
                })
            }
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        if !self.enabled {
            return;
        }
        self.server.service_ready(now, |_| {});
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
}
