/*
Time-queue for the performance model.

This module should provide scheduling helpers which sit between the functional simulator and the
performance model.  The goal is to keep the simulator structure as is and have the time-queue handle
the low-level details of latency, bandwidth, and backpressure.

Each shared resource is wrapped by a TimedServer, which enforced a service law:
    - A base latency plus a throughput component expressed in bytes-per-cycle

When the server cannot immediately accept more work it returns a Backpressure, which allows
the caller to propagate the stall reason back to the warp or unit that produced the request.

Accepted requests yield a `Ticket` describing when the service will complete. Downstream components
can poll these tickets (or register callbacks) to model the flow of work through the pipeline.
*/

use crate::base::module::IsModule;
use std::collections::VecDeque;

pub type Cycle = u64;

// Helper to read the current cycle from any Cyclotron module
pub fn module_now<M: IsModule>(module: &M) -> Cycle {
    module.base_ref().cycle
}

// Result of queueing a request with a timed server
#[derive(Debug, Clone, Copy)]
pub struct Ticket {
    issued_at: Cycle,
    ready_at: Cycle,
    size_bytes: u32,
}

impl Ticket {
    fn new(issued_at: Cycle, ready_at: Cycle, size_bytes: u32) -> Self {
        Self {
            issued_at,
            ready_at,
            size_bytes,
        }
    }

    // Cycle at which the request entered the server.
    pub fn issued_at(&self) -> Cycle {
        self.issued_at
    }

    // Cycle at which the server will make the payload available to downstream consumers.
    pub fn ready_at(&self) -> Cycle {
        self.ready_at
    }

    // Size of the request
    pub fn size_bytes(&self) -> u32 {
        self.size_bytes
    }

    // Whether the ticket is ready at the provided cycle.
    pub fn is_ready(&self, now: Cycle) -> bool {
        now >= self.ready_at
    }

    // Number of cycles until the ticket is ready.  Returns zero if already ready.
    pub fn remaining_cycles(&self, now: Cycle) -> Cycle {
        self.ready_at.saturating_sub(now)
    }
}

// The request carries the payload and metadata that is required to compute the service time
#[derive(Debug)]
pub struct ServiceRequest<T> {
    pub payload: T,
    pub size_bytes: u32,
}

impl<T> ServiceRequest<T> {
    pub fn new(payload: T, size_bytes: u32) -> Self {
        Self {
            payload,
            size_bytes,
        }
    }
}

#[derive(Debug)]
pub struct ServiceResult<T> {
    pub payload: T,
    pub ticket: Ticket,
}

// Reasons why the server rejected a request
#[derive(Debug)]
pub enum Backpressure<T> {
    // The bounded FIFO is full
    QueueFull {
        request: ServiceRequest<T>,
        capacity: usize,
    },
    // The server is currently busy (e.g. warm-up latency with no queued requests)
    Busy {
        request: ServiceRequest<T>,
        available_at: Cycle,
    },
}

impl<T> Backpressure<T> {
    // Recover the underlying request so it can be retried later.
    pub fn into_request(self) -> ServiceRequest<T> {
        match self {
            Backpressure::QueueFull { request, .. } => request,
            Backpressure::Busy { request, .. } => request,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ServerConfig {
    // Fixed latency added to every request
    pub base_latency: Cycle,
    // Throughput
    pub bytes_per_cycle: u32,
    // Maximum number of outstanding requests the server will accept
    pub queue_capacity: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            base_latency: 0,
            bytes_per_cycle: 1,
            queue_capacity: 1,
        }
    }
}

#[derive(Debug)]
struct Inflight<T> {
    payload: T,
    ticket: Ticket,
}

// Single-lane server that enforces the configured latency/bandwidth budget and keeps track of
// outstanding work using a FIFO.
#[derive(Debug)]
pub struct TimedServer<T> {
    config: ServerConfig,
    inflight: VecDeque<Inflight<T>>,
    busy_until: Cycle,
}

impl<T> TimedServer<T> {
    pub fn new(config: ServerConfig) -> Self {
        assert!(config.bytes_per_cycle > 0, "bytes_per_cycle must be > 0");
        assert!(config.queue_capacity > 0, "queue_capacity must be > 0");
        Self {
            config,
            inflight: VecDeque::with_capacity(config.queue_capacity),
            busy_until: 0,
        }
    }

    // Attempt to enqueue a request at the provided cycle.
    // Returns a Ticket on success or a Backpressure describing why the request could not be accepted.
    pub fn try_enqueue(
        &mut self,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>> {
        if self.inflight.len() >= self.config.queue_capacity {
            return Err(Backpressure::QueueFull {
                request,
                capacity: self.config.queue_capacity,
            });
        }

        let available_at = self.busy_until.max(now);
        if available_at > now && self.inflight.is_empty() {
            return Err(Backpressure::Busy {
                request,
                available_at,
            });
        }

        let ready_at = self.next_ready_cycle(available_at, request.size_bytes);
        let ticket = Ticket::new(now, ready_at, request.size_bytes);

        self.busy_until = ready_at;
        self.inflight.push_back(Inflight {
            payload: request.payload,
            ticket,
        });

        Ok(ticket)
    }

    // Drain any requests that have completed by "now" and invoke the supplied callback with the
    // results.
    pub fn service_ready<F>(&mut self, now: Cycle, mut callback: F)
    where
        F: FnMut(ServiceResult<T>),
    {
        while let Some(front) = self.inflight.front() {
            if !front.ticket.is_ready(now) {
                break;
            }
            let inflight = self.inflight.pop_front().expect("front just checked");
            callback(ServiceResult {
                payload: inflight.payload,
                ticket: inflight.ticket,
            });
        }

        if self.inflight.is_empty() && now > self.busy_until {
            self.busy_until = now;
        }
    }

    // Returns the earliest cycle at which a new request could begin service.
    pub fn available_at(&self) -> Cycle {
        self.busy_until
    }

    pub fn oldest_ticket(&self) -> Option<&Ticket> {
        self.inflight.front().map(|inflight| &inflight.ticket)
    }

    fn next_ready_cycle(&self, start: Cycle, size_bytes: u32) -> Cycle {
        let service_cycles = ceil_div_u64(size_bytes as u64, self.config.bytes_per_cycle as u64);
        start
            .saturating_add(self.config.base_latency)
            .saturating_add(service_cycles)
    }
}

fn ceil_div_u64(nom: u64, denom: u64) -> Cycle {
    debug_assert!(denom > 0);
    (nom + denom - 1) / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server(config: ServerConfig) -> TimedServer<&'static str> {
        TimedServer::new(config)
    }

    #[test]
    // Make sure the service time calculation combines both the base latency and the bandwidth
    fn ticket_ready_time_respects_latency_and_bandwidth() {
        let mut server = make_server(ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 4,
            queue_capacity: 4,
        });

        let ticket = server
            .try_enqueue(0, ServiceRequest::new("req0", 8))
            .expect("first request should be accepted");

        assert_eq!(0, ticket.issued_at());
        assert_eq!(8, ticket.size_bytes());
        // service time = base_latency (3) + ceil(8 / 4) = 5 cycles total
        assert_eq!(5, ticket.ready_at());

        // second request queues behind the first and inherits the busy_until window
        let ticket1 = server
            .try_enqueue(1, ServiceRequest::new("req1", 4))
            .expect("second request should be accepted");
        assert_eq!(9, ticket1.ready_at());
    }

    #[test]
    // make sure that the server rejects requests when the queue is full
    fn queue_full_is_reported_when_capacity_reached() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 8,
            queue_capacity: 1,
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 16))
            .expect("first request should be accepted");

        match server.try_enqueue(0, ServiceRequest::new("req1", 16)) {
            Err(Backpressure::QueueFull { capacity, .. }) => assert_eq!(1, capacity),
            other => panic!("expected queue full backpressure, got {other:?}"),
        }
    }

    #[test]
    // make sure that completed requests are properly removed from the queue
    fn service_ready_drains_completed_requests() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();
        server
            .try_enqueue(1, ServiceRequest::new("req1", 4))
            .unwrap();

        let mut collected = Vec::new();
        server.service_ready(1, |res| collected.push(res.payload));
        assert!(collected.is_empty(), "no requests ready yet");

        server.service_ready(3, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        server.service_ready(4, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);

        assert_eq!(4, server.available_at());
    }

    #[test]
    // make sure that servers can model warm up delays
    // this will expect the Busy backpressure as the queue is empty but the server is not ready
    fn busy_backpressure_allows_warmup_modeling() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 2,
        });

        // Simulate a warm-up delay before the first request can launch.
        server.busy_until = 5;

        match server.try_enqueue(0, ServiceRequest::new("req0", 4)) {
            Err(Backpressure::Busy { available_at, .. }) => assert_eq!(5, available_at),
            other => panic!("expected busy backpressure, got {other:?}"),
        }

        let ticket = server
            .try_enqueue(6, ServiceRequest::new("req0", 4))
            .expect("server should accept once warm-up passes");
        assert_eq!(7, ticket.ready_at());
    }

    #[test]
    // make sure that zero-byte requests only incur base latency and no throughput delay
    fn zero_byte_requests_complete_with_base_latency_only() {
        let mut server = make_server(ServerConfig {
            base_latency: 5,
            bytes_per_cycle: 8,
            queue_capacity: 4,
        });

        let ticket = server
            .try_enqueue(10, ServiceRequest::new("zero_bytes", 0))
            .expect("zero byte request should be accepted");

        assert_eq!(10, ticket.issued_at());
        assert_eq!(0, ticket.size_bytes());
        // Service time = base_latency (5) + ceil(0/8) = 5 + 0 = 5 cycles
        assert_eq!(15, ticket.ready_at());

        // Second zero-byte request should queue behind the first
        let ticket2 = server
            .try_enqueue(11, ServiceRequest::new("zero_bytes2", 0))
            .expect("second zero byte request should be accepted");
        assert_eq!(20, ticket2.ready_at());
    }

    #[test]
    // if we call service_ready() multiple times at the same cycle, it doesnt double drain the FIFO
    fn multiple_service_ready_calls_are_idempotent() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();
        server
            .try_enqueue(1, ServiceRequest::new("req1", 4))
            .unwrap();

        let mut collected = Vec::new();

        // First call at cycle 3 should drain req0
        server.service_ready(3, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        // Second call at same cycle should be idempotent
        server.service_ready(3, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        // Call at cycle 4 should drain req1
        server.service_ready(4, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);

        // Multiple calls at cycle 5+ should be idempotent
        server.service_ready(5, |res| collected.push(res.payload));
        server.service_ready(6, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);
    }

    #[test]
    // make sure rejected requests can be retried later
    fn requests_can_be_retried_after_backpressure() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 1,
        });

        // Fill the queue with one request
        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .expect("first request should be accepted");

        // Second request should be rejected with QueueFull
        let rejected_request = match server.try_enqueue(0, ServiceRequest::new("req1", 8)) {
            Err(backpressure) => backpressure.into_request(),
            Ok(_) => panic!("expected queue full backpressure"),
        };

        // Verify the rejected request can be recovered
        assert_eq!("req1", rejected_request.payload);
        assert_eq!(8, rejected_request.size_bytes);

        // Drain the completed request to make space
        let mut drained = Vec::new();
        server.service_ready(3, |res| drained.push(res.payload));
        assert_eq!(vec!["req0"], drained);

        // Now retry the previously rejected request
        let ticket = server
            .try_enqueue(3, rejected_request)
            .expect("retried request should be accepted after queue drains");
        assert_eq!(6, ticket.ready_at()); // 3 + 1 + ceil(8/4) = 6
    }

    #[test]
    // rejected requests due to Busy signal can also be retried later
    fn busy_backpressure_can_also_be_retried() {
        let mut server = make_server(ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 2,
        });

        // Set server to be busy until cycle 10
        server.busy_until = 10;

        // Request at cycle 5 should be rejected with Busy
        let rejected_request = match server.try_enqueue(5, ServiceRequest::new("busy_req", 4)) {
            Err(Backpressure::Busy {
                request,
                available_at,
            }) => {
                assert_eq!(10, available_at);
                request
            }
            other => panic!("expected busy backpressure, got {other:?}"),
        };

        // Retry the request after the busy period
        let ticket = server
            .try_enqueue(10, rejected_request)
            .expect("retried request should be accepted after busy period");
        assert_eq!(13, ticket.ready_at()); // 10 + 2 + ceil(4/4) = 13
    }

    #[test]
    // if we have huge cycle numbers, make sure we don't overflow when computing ready_at
    fn large_cycles_dont_overflow() {
        let mut server = make_server(ServerConfig {
            base_latency: 1000,
            bytes_per_cycle: 1,
            queue_capacity: 4,
        });

        // Use a very large cycle number close to u64::MAX
        let large_cycle = u64::MAX - 2000;

        let ticket = server
            .try_enqueue(large_cycle, ServiceRequest::new("large_cycle", 100))
            .expect("large cycle request should be accepted");

        assert_eq!(large_cycle, ticket.issued_at());
        // Service time = 1000 + ceil(100/1) = 1100
        // Should not overflow: large_cycle + 1100 should be valid
        assert!(ticket.ready_at() > large_cycle);
        assert!(ticket.ready_at() <= u64::MAX);
    }

    #[test]
    // make sure arithmetic operations saturate rather than overflows
    fn saturating_arithmetic_prevents_overflow() {
        let mut server = make_server(ServerConfig {
            base_latency: 1000,
            bytes_per_cycle: 1,
            queue_capacity: 4,
        });

        // Set busy_until very close to u64::MAX
        server.busy_until = u64::MAX - 500;

        // Request at a time when server is not busy (after busy_until)
        // This should saturate rather than overflow
        let ticket = server
            .try_enqueue(u64::MAX - 400, ServiceRequest::new("near_max", 1000))
            .expect("near max cycle request should be accepted when server not busy");

        // The ready_at should be u64::MAX due to saturating_add
        assert_eq!(u64::MAX, ticket.ready_at());
    }

    #[test]
    // some edge cases for remaining_cycles (exactly ready, past ready)
    fn ticket_remaining_cycles_handles_edge_cases() {
        let ticket = Ticket::new(100, 150, 32);

        // Normal case: ticket not ready yet
        assert_eq!(20, ticket.remaining_cycles(130));

        // Edge case: exactly ready
        assert_eq!(0, ticket.remaining_cycles(150));

        // Edge case: past ready time (should saturate to 0)
        assert_eq!(0, ticket.remaining_cycles(200));

        // Edge case: way past ready time
        assert_eq!(0, ticket.remaining_cycles(u64::MAX));
    }
}
