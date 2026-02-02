use crate::timeflow::types::{Reject, RejectWith, RejectReason};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

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

    pub fn try_issue(&mut self, now: Cycle, payload: T, bytes: u32) -> Result<Ticket, Reject> {
        if !self.enabled {
            return Ok(Ticket::synthetic(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy { available_at, .. }) => Err(Reject {
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
                Err(Reject {
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
    ) -> Result<Ticket, RejectWith<T>> {
        if !self.enabled {
            return Ok(Ticket::synthetic(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy {
                request,
                available_at,
            }) => Err(RejectWith {
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
                Err(RejectWith {
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
