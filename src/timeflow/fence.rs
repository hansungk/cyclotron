use std::collections::VecDeque;

use serde::Deserialize;

use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub struct FenceRequest {
    pub warp: usize,
    pub request_id: u64,
}

#[derive(Debug, Clone)]
pub struct FenceIssue {
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct FenceReject {
    pub retry_at: Cycle,
    pub reason: FenceRejectReason,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FenceConfig {
    pub enabled: bool,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for FenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            queue: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 1,
                queue_capacity: 4,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
        }
    }
}

pub struct FenceQueue {
    enabled: bool,
    server: TimedServer<FenceRequest>,
    ready: VecDeque<FenceRequest>,
}

impl FenceQueue {
    pub fn new(config: FenceConfig) -> Self {
        Self {
            enabled: config.enabled,
            server: TimedServer::new(config.queue),
            ready: VecDeque::new(),
        }
    }

    pub fn try_issue(
        &mut self,
        now: Cycle,
        request: FenceRequest,
    ) -> Result<FenceIssue, FenceReject> {
        if !self.enabled {
            self.ready.push_back(request);
            return Ok(FenceIssue {
                ticket: Ticket::synthetic(now, now, 0),
            });
        }

        let request = ServiceRequest::new(request, 0);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(FenceIssue { ticket }),
            Err(Backpressure::Busy { available_at, .. }) => Err(FenceReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: FenceRejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(FenceReject {
                    retry_at,
                    reason: FenceRejectReason::QueueFull,
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

    pub fn pop_ready(&mut self) -> Option<FenceRequest> {
        self.ready.pop_front()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::{FenceConfig, FenceQueue, FenceRequest};

    #[test]
    fn fence_queue_delays_release() {
        let mut cfg = FenceConfig::default();
        cfg.enabled = true;
        cfg.queue.base_latency = 2;
        cfg.queue.queue_capacity = 1;
        let mut fence = FenceQueue::new(cfg);

        assert!(fence
            .try_issue(0, FenceRequest { warp: 0, request_id: 1 })
            .is_ok());
        fence.tick(1);
        assert!(fence.pop_ready().is_none());
        fence.tick(2);
        assert!(fence.pop_ready().is_some());
    }
}
