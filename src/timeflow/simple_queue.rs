use crate::timeflow::types::RejectReason;
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub struct SimpleReject {
    pub retry_at: Cycle,
    pub reason: RejectReason,
}

#[derive(Debug, Clone)]
pub struct SimpleRejectWith<T> {
    pub retry_at: Cycle,
    pub reason: RejectReason,
    pub payload: T,
}

pub struct SimpleTimedQueue<T> {
    enabled: bool,
    server: TimedServer<T>,
}

impl<T> SimpleTimedQueue<T> {
    pub fn new(enabled: bool, queue: ServerConfig) -> Self {
        Self {
            enabled,
            server: TimedServer::new(queue),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn try_issue(&mut self, now: Cycle, payload: T, bytes: u32) -> Result<Ticket, SimpleReject> {
        if !self.enabled {
            return Ok(Ticket::synthetic(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy { available_at, .. }) => Err(SimpleReject {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: RejectReason::Busy,
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(SimpleReject {
                    retry_at,
                    reason: RejectReason::QueueFull,
                })
            }
        }
    }

    pub fn try_issue_with_payload(
        &mut self,
        now: Cycle,
        payload: T,
        bytes: u32,
    ) -> Result<Ticket, SimpleRejectWith<T>> {
        if !self.enabled {
            return Ok(Ticket::synthetic(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy {
                request,
                available_at,
            }) => Err(SimpleRejectWith {
                retry_at: available_at.max(now.saturating_add(1)),
                reason: RejectReason::Busy,
                payload: request.payload,
            }),
            Err(Backpressure::QueueFull { request, .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at())
                    .max(now.saturating_add(1));
                Err(SimpleRejectWith {
                    retry_at,
                    reason: RejectReason::QueueFull,
                    payload: request.payload,
                })
            }
        }
    }

    pub fn tick<R>(&mut self, now: Cycle, mut on_ready: R)
    where
        R: FnMut(T),
    {
        if !self.enabled {
            return;
        }
        self.server.service_ready(now, |result| {
            on_ready(result.payload);
        });
    }
}
