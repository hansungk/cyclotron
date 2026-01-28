use std::collections::VecDeque;

use serde::Deserialize;

use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritebackRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct WritebackReject {
    pub retry_at: Cycle,
    pub reason: WritebackRejectReason,
}

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
    enabled: bool,
    server: TimedServer<WritebackPayload>,
    ready: VecDeque<WritebackPayload>,
}

impl WritebackQueue {
    pub fn new(config: WritebackConfig) -> Self {
        let mut cfg = config.queue;
        cfg.base_latency = cfg.base_latency.max(0);
        Self {
            enabled: config.enabled,
            server: TimedServer::new(cfg),
            ready: VecDeque::new(),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        payload: WritebackPayload,
    ) -> Result<WritebackIssue, WritebackReject> {
        if !self.enabled {
            self.ready.push_back(payload);
            return Ok(WritebackIssue {
                ticket: Ticket::synthetic(now, now, 0),
            });
        }

        let request = ServiceRequest::new(payload, 0);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(WritebackIssue { ticket }),
            Err(Backpressure::Busy { available_at, .. }) => Err(WritebackReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: WritebackRejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(WritebackReject {
                    retry_at,
                    reason: WritebackRejectReason::QueueFull,
                })
            }
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        if !self.enabled {
            return;
        }
        self.server.service_ready(now, |result| {
            self.ready.push_back(result.payload);
        });
    }

    pub fn pop_ready(&mut self) -> Option<WritebackPayload> {
        self.ready.pop_front()
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::{WritebackConfig, WritebackPayload, WritebackQueue};
    use crate::timeflow::gmem::GmemCompletion;
    use crate::timeflow::gmem::GmemRequest;

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
}
