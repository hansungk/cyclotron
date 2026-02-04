use crate::timeflow::types::{Reject, RejectReason, RejectWith};
use crate::timeq::{
    normalize_retry, Backpressure, Cycle, ServerConfig, ServiceRequest, ServiceResult, Ticket,
    TimedServer,
};

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
            return Ok(Ticket::new(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy { available_at, .. }) => Err(Reject::new(
                normalize_retry(now, available_at),
                RejectReason::Busy,
            )),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self.queue_full_retry(now);
                Err(Reject::new(retry_at, RejectReason::QueueFull))
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
            return Ok(Ticket::new(now, now, bytes));
        }

        let request = ServiceRequest::new(payload, bytes);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket),
            Err(Backpressure::Busy {
                request,
                available_at,
            }) => {
                let retry_at = normalize_retry(now, available_at);
                Err(RejectWith::new(
                    request.payload,
                    retry_at,
                    RejectReason::Busy,
                ))
            }
            Err(Backpressure::QueueFull { request, .. }) => {
                let retry_at = self.queue_full_retry(now);
                Err(RejectWith::new(
                    request.payload,
                    retry_at,
                    RejectReason::QueueFull,
                ))
            }
        }
    }

    fn queue_full_retry(&self, now: Cycle) -> Cycle {
        let suggested = self
            .server
            .oldest_ticket()
            .map(|ticket| ticket.ready_at())
            .unwrap_or_else(|| self.server.available_at());
        normalize_retry(now, suggested)
    }

    pub fn tick_with_service_result<R>(&mut self, now: Cycle, mut on_ready: R)
    where
        R: FnMut(ServiceResult<T>),
    {
        if !self.enabled {
            return;
        }
        self.server.service_ready(now, |result| {
            on_ready(result);
        });
    }

    pub fn tick<R>(&mut self, now: Cycle, mut on_ready: R)
    where
        R: FnMut(T),
    {
        self.tick_with_service_result(now, |result| {
            on_ready(result.payload);
        });
    }

    pub fn outstanding(&self) -> usize {
        self.server.outstanding()
    }

    pub fn is_busy(&self) -> bool {
        self.enabled && self.server.outstanding() > 0
    }
}
