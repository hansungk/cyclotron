use crate::base::module::IsModule;
use serde::Deserialize;
use std::collections::VecDeque;

pub type Cycle = u64;

/// Normalize a suggested retry cycle by ensuring it's strictly in the future
/// (at least `now + 1`). Providers should call this when they have a
/// suggested `available_at` to produce a `retry_at` for callers.
pub fn normalize_retry(now: Cycle, suggested: Cycle) -> Cycle {
    suggested.max(now.saturating_add(1))
}
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

    pub(crate) fn synthetic(issued_at: Cycle, ready_at: Cycle, size_bytes: u32) -> Self {
        Self {
            issued_at,
            ready_at,
            size_bytes,
        }
    }

    // Cycle at which the request is issued at
    pub fn issued_at(&self) -> Cycle {
        self.issued_at
    }

    // Cycle at which the server will make the payload available to downstream consumers
    pub fn ready_at(&self) -> Cycle {
        self.ready_at
    }

    // Size of the request
    pub fn size_bytes(&self) -> u32 {
        self.size_bytes
    }

    // Whether the ticket is ready at the provided cycle
    pub fn is_ready(&self, now: Cycle) -> bool {
        now >= self.ready_at
    }

    // Number of cycles until the ticket is ready (returns zero if already ready)
    pub fn remaining_cycles(&self, now: Cycle) -> Cycle {
        self.ready_at.saturating_sub(now)
    }
}

// The request carries the payload and metadata required to compute the service time
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
    // The server is currently busy due to some warm-up latency
    Busy {
        request: ServiceRequest<T>,
        available_at: Cycle,
    },
}

impl<T> Backpressure<T> {
    // Recover the underlying request so it can be retried later
    pub fn into_request(self) -> ServiceRequest<T> {
        match self {
            Backpressure::QueueFull { request, .. } => request,
            Backpressure::Busy { request, .. } => request,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    // Fixed latency added to every request
    pub base_latency: Cycle,
    // Throughput
    pub bytes_per_cycle: u32,
    // Maximum number of outstanding requests the server will accept
    pub queue_capacity: usize,
    // Maximum number of completions exposed per cycle (u32::MAX = unlimited)
    pub completions_per_cycle: u32,
    // Optional warm-up latency before the first request can issue
    pub warmup_latency: Cycle,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            base_latency: 0,
            bytes_per_cycle: 1,
            queue_capacity: 1,
            completions_per_cycle: u32::MAX,
            warmup_latency: 0,
        }
    }
}

#[derive(Debug)]
struct Inflight<T> {
    payload: T,
    ticket: Ticket,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ServerStats {
    pub issued: u64,
    pub completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
    pub bytes_issued: u64,
    pub bytes_completed: u64,
    pub max_outstanding: u64,
}

// Single-lane server that enforces the configured latency/bandwidth budget and
// keeps track of outstanding work
#[derive(Debug)]
pub struct TimedServer<T> {
    config: ServerConfig,
    inflight: VecDeque<Inflight<T>>,
    ready: VecDeque<ServiceResult<T>>,
    next_issue_at: Cycle,
    warmup_until: Cycle,
    last_completion_cycle: Cycle,
    completions_this_cycle: u32,
    stats: ServerStats,
}

impl<T> TimedServer<T> {
    pub fn new(config: ServerConfig) -> Self {
        assert!(config.bytes_per_cycle > 0, "bytes_per_cycle must be > 0");
        assert!(config.queue_capacity > 0, "queue_capacity must be > 0");
        assert!(
            config.completions_per_cycle > 0,
            "completions_per_cycle must be > 0"
        );
        Self {
            config,
            inflight: VecDeque::with_capacity(config.queue_capacity),
            ready: VecDeque::with_capacity(config.queue_capacity),
            next_issue_at: 0,
            warmup_until: config.warmup_latency,
            last_completion_cycle: 0,
            completions_this_cycle: 0,
            stats: ServerStats::default(),
        }
    }

    // Attempt to enqueue a request at the provided cycle
    // Returns a Ticket on success or a Backpressure describing why the request could not be accepted
    pub fn try_enqueue(
        &mut self,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>> {
        if self.outstanding_len() >= self.config.queue_capacity {
            self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
            return Err(Backpressure::QueueFull {
                request,
                capacity: self.config.queue_capacity,
            });
        }

        if self.outstanding_len() == 0 && now < self.warmup_until {
            self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
            return Err(Backpressure::Busy {
                request,
                available_at: self.warmup_until,
            });
        }

        let start = self.next_issue_at.max(now);
        let service_cycles = ceil_div_u64(
            request.size_bytes as u64,
            self.config.bytes_per_cycle as u64,
        );
        let ready_at = start
            .saturating_add(self.config.base_latency)
            .saturating_add(service_cycles);
        let ticket = Ticket::new(now, ready_at, request.size_bytes);

        self.next_issue_at = start.saturating_add(service_cycles);
        self.inflight.push_back(Inflight {
            payload: request.payload,
            ticket,
        });
        self.stats.issued = self.stats.issued.saturating_add(1);
        self.stats.bytes_issued = self
            .stats
            .bytes_issued
            .saturating_add(request.size_bytes as u64);
        let outstanding = self.outstanding_len() as u64;
        self.stats.max_outstanding = self.stats.max_outstanding.max(outstanding);

        Ok(ticket)
    }

    // Drain any requests that have completed and invoke the callback with the results
    pub fn service_ready<F>(&mut self, now: Cycle, mut callback: F)
    where
        F: FnMut(ServiceResult<T>),
    {
        self.advance_ready(now);
        while let Some(result) = self.ready.pop_front() {
            callback(result);
        }
        self.update_idle(now);
    }

    // Returns the earliest cycle that a new request can begin service
    pub fn available_at(&self) -> Cycle {
        self.next_issue_at.max(self.warmup_until)
    }

    pub fn oldest_ticket(&self) -> Option<&Ticket> {
        self.inflight.front().map(|inflight| &inflight.ticket)
    }

    // Make any newly ready completions visible to downstream consumers without consuming them
    pub fn advance_ready(&mut self, now: Cycle) {
        if self.last_completion_cycle != now {
            self.last_completion_cycle = now;
            self.completions_this_cycle = 0;
        }
        let cap = self.config.completions_per_cycle;
        while let Some(front) = self.inflight.front() {
            if !front.ticket.is_ready(now) {
                break;
            }
            if self.completions_this_cycle >= cap {
                break;
            }
            let inflight = self.inflight.pop_front().expect("front just checked");
            self.ready.push_back(ServiceResult {
                payload: inflight.payload,
                ticket: inflight.ticket,
            });
            self.completions_this_cycle = self.completions_this_cycle.saturating_add(1);
            self.stats.completed = self.stats.completed.saturating_add(1);
            self.stats.bytes_completed = self
                .stats
                .bytes_completed
                .saturating_add(inflight.ticket.size_bytes() as u64);
        }
    }

    // Observe the next ready completion without consuming it
    pub fn peek_ready(&mut self, now: Cycle) -> Option<&ServiceResult<T>> {
        self.advance_ready(now);
        self.ready.front()
    }

    // Consume and return the next ready completion if one exists
    pub fn pop_ready(&mut self, now: Cycle) -> Option<ServiceResult<T>> {
        self.advance_ready(now);
        let result = self.ready.pop_front();
        if result.is_some() {
            self.update_idle(now);
        }
        result
    }

    // Number of outstanding requests (queued, inflight, or waiting to be drained)
    pub fn outstanding(&self) -> usize {
        self.outstanding_len()
    }

    fn outstanding_len(&self) -> usize {
        self.inflight.len() + self.ready.len()
    }

    fn update_idle(&mut self, now: Cycle) {
        if self.inflight.is_empty() && self.ready.is_empty() && now > self.next_issue_at {
            self.next_issue_at = now;
        }
    }

    pub fn stats(&self) -> ServerStats {
        self.stats
    }

    pub fn clear_stats(&mut self) {
        self.stats = ServerStats::default();
    }

    pub fn set_warmup_until(&mut self, cycle: Cycle) {
        self.warmup_until = cycle;
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
            ..ServerConfig::default()
        });

        let ticket = server
            .try_enqueue(0, ServiceRequest::new("req0", 8))
            .expect("first request should be accepted");

        assert_eq!(0, ticket.issued_at());
        assert_eq!(8, ticket.size_bytes());
        // service time = base_latency (3) + ceil(8 / 4) = 5 cycles total
        assert_eq!(5, ticket.ready_at());
        // second request pipelines one cycle behind the first
        let ticket1 = server
            .try_enqueue(1, ServiceRequest::new("req1", 4))
            .expect("second request should be accepted");
        assert_eq!(6, ticket1.ready_at());
    }

    #[test]
    // Make sure that the server rejects requests when the queue is full
    fn queue_full_is_reported_when_capacity_reached() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 8,
            queue_capacity: 1,
            ..ServerConfig::default()
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
    // Make sure that completed requests are properly removed from the queue
    fn service_ready_drains_completed_requests() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
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

        server.service_ready(2, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        server.service_ready(3, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);

        assert_eq!(3, server.available_at());
    }

    #[test]
    // Make sure that servers can model warm up delays
    // This will expect the Busy backpressure as the queue is empty but the server is not ready
    fn busy_backpressure_allows_warmup_modeling() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });

        server.set_warmup_until(5);

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
    // Make sure that zero-byte requests only incur base latency and no throughput delay
    fn zero_byte_requests_complete_with_base_latency_only() {
        let mut server = make_server(ServerConfig {
            base_latency: 5,
            bytes_per_cycle: 8,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        let ticket = server
            .try_enqueue(10, ServiceRequest::new("zero_bytes", 0))
            .expect("zero byte request should be accepted");

        assert_eq!(10, ticket.issued_at());
        assert_eq!(0, ticket.size_bytes());
        // Service time = base_latency (5) + ceil(0/8) = 5 + 0 = 5 cycles
        assert_eq!(15, ticket.ready_at());

        // Second zero-byte request should pipeline right after the first
        let ticket2 = server
            .try_enqueue(11, ServiceRequest::new("zero_bytes2", 0))
            .expect("second zero byte request should be accepted");
        assert_eq!(16, ticket2.ready_at());
    }

    #[test]
    // If we call service_ready() multiple times at the same cycle, it doesnt double drain the FIFO
    fn multiple_service_ready_calls_are_idempotent() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();
        server
            .try_enqueue(1, ServiceRequest::new("req1", 4))
            .unwrap();

        let mut collected = Vec::new();

        // First call at cycle 2 should drain req0
        server.service_ready(2, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        // Second call at same cycle should be idempotent
        server.service_ready(2, |res| collected.push(res.payload));
        assert_eq!(vec!["req0"], collected);

        // Call at cycle 3 should drain req1
        server.service_ready(3, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);

        // Multiple calls at cycle 5+ should be idempotent
        server.service_ready(5, |res| collected.push(res.payload));
        server.service_ready(6, |res| collected.push(res.payload));
        assert_eq!(vec!["req0", "req1"], collected);
    }

    #[test]
    // Make sure rejected requests can be retried later
    fn requests_can_be_retried_after_backpressure() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 1,
            ..ServerConfig::default()
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
    fn variable_size_requests_compute_correct_service_time() {
        let mut server = make_server(ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        });

        let sizes = [1u32, 4, 16, 64, 128];
        for (idx, size) in sizes.iter().enumerate() {
            let now = (idx as u64) * 100;
            let ticket = server
                .try_enqueue(now, ServiceRequest::new("req", *size))
                .expect("request should be accepted");
            let service = ((*size as u64) + 3) / 4;
            let expected = now + 3 + service;
            assert_eq!(expected, ticket.ready_at());
        }
    }

    #[test]
    fn enqueue_at_exact_warmup_boundary() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });
        server.set_warmup_until(100);

        let ticket = server
            .try_enqueue(100, ServiceRequest::new("req", 4))
            .expect("enqueue at warmup boundary should succeed");
        assert_eq!(102, ticket.ready_at());
    }

    #[test]
    fn completions_per_cycle_equals_one_with_many_ready() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 8,
            queue_capacity: 16,
            completions_per_cycle: 1,
            ..ServerConfig::default()
        });

        for _ in 0..5 {
            server
                .try_enqueue(0, ServiceRequest::new("req", 0))
                .expect("enqueue should succeed");
        }

        server.advance_ready(0);
        let mut ready = Vec::new();
        server.service_ready(0, |res| ready.push(res.payload));
        assert_eq!(1, ready.len(), "only one completion per cycle");
        server.service_ready(0, |res| ready.push(res.payload));
        assert_eq!(1, ready.len(), "same cycle should not drain more");
    }

    #[test]
    fn advance_ready_moves_to_ready_queue() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        let ticket = server
            .try_enqueue(0, ServiceRequest::new("req", 4))
            .expect("enqueue should succeed");
        assert!(server.peek_ready(0).is_none());
        server.advance_ready(ticket.ready_at());
        assert!(server.peek_ready(ticket.ready_at()).is_some());
    }

    #[test]
    fn back_to_back_requests_pipeline_correctly() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        });

        let mut ready = Vec::new();
        for cycle in 0..5 {
            let ticket = server
                .try_enqueue(cycle, ServiceRequest::new("req", 4))
                .expect("enqueue should succeed");
            ready.push(ticket.ready_at());
        }

        assert_eq!(ready, vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn single_byte_request_at_cycle_zero() {
        let mut server = make_server(ServerConfig {
            base_latency: 5,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        let ticket = server
            .try_enqueue(0, ServiceRequest::new("req", 1))
            .expect("enqueue should succeed");
        assert_eq!(6, ticket.ready_at());
    }

    #[test]
    fn max_capacity_then_drain_one_then_enqueue() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });

        server.try_enqueue(0, ServiceRequest::new("a", 4)).unwrap();
        server.try_enqueue(0, ServiceRequest::new("b", 4)).unwrap();
        assert!(server.try_enqueue(0, ServiceRequest::new("c", 4)).is_err());

        server.advance_ready(1);
        assert!(server.pop_ready(1).is_some());
        assert!(server.try_enqueue(1, ServiceRequest::new("c", 4)).is_ok());
    }

    #[test]
    fn high_throughput_saturated_queue() {
        let mut server = TimedServer::<u32>::new(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 1024,
            queue_capacity: 1024,
            ..ServerConfig::default()
        });

        for id in 0..1024u32 {
            server
                .try_enqueue(0, ServiceRequest::new(id, 1))
                .expect("enqueue should succeed");
        }

        let mut seen = Vec::new();
        for cycle in 1..=1024u64 {
            while let Some(result) = server.pop_ready(cycle) {
                seen.push(result.payload);
            }
        }
        assert_eq!(seen.len(), 1024);
        for (idx, val) in seen.into_iter().enumerate() {
            assert_eq!(val as usize, idx);
        }
    }

    #[test]
    // Rejected requests due to Busy signal can be retried later
    fn busy_backpressure_can_also_be_retried() {
        let mut server = make_server(ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });

        server.set_warmup_until(10);

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
    // If we have huge cycle numbers, make sure we don't overflow when computing ready_at
    fn large_cycles_dont_overflow() {
        let mut server = make_server(ServerConfig {
            base_latency: 1000,
            bytes_per_cycle: 1,
            queue_capacity: 4,
            ..ServerConfig::default()
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
    // Make sure arithmetic operations saturate rather than overflows
    fn saturating_arithmetic_prevents_overflow() {
        let mut server = make_server(ServerConfig {
            base_latency: 1000,
            bytes_per_cycle: 1,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        let ticket = server
            .try_enqueue(u64::MAX - 400, ServiceRequest::new("near_max", 1000))
            .expect("near max cycle request should be accepted when server not busy");

        assert_eq!(u64::MAX, ticket.ready_at());
    }

    #[test]
    // Some edge cases for remaining_cycles (exactly ready, past ready)
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

    #[test]
    // Verify that peek_ready allows observing completions without consuming them,
    // and pop_ready actually removes them from the queue
    fn peek_and_pop_ready_allow_backpressure_control() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();

        // Nothing ready yet.
        assert!(server.peek_ready(1).is_none());

        // Once the request matures, peek exposes it without consuming.
        let peeked = server.peek_ready(5).expect("result should be ready");
        assert_eq!("req0", peeked.payload);

        // The item stays pending until pop_ready is invoked.
        assert!(server.peek_ready(5).is_some());

        let popped = server
            .pop_ready(5)
            .expect("result should be popped successfully");
        assert_eq!("req0", popped.payload);

        // After popping, there is no outstanding work.
        assert!(server.pop_ready(5).is_none());
        assert_eq!(server.outstanding(), 0);
    }

    #[test]
    // Ensure that multiple requests can be in-flight simultaneously and pipeline correctly,
    // with each request starting service as soon as throughput allows
    fn pipelined_requests_overlap() {
        let mut server = make_server(ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        });

        let t0 = server
            .try_enqueue(0, ServiceRequest::new("req0", 8))
            .unwrap();
        let t1 = server
            .try_enqueue(1, ServiceRequest::new("req1", 8))
            .unwrap();
        let t2 = server
            .try_enqueue(2, ServiceRequest::new("req2", 8))
            .unwrap();

        assert_eq!(0, t0.issued_at());
        assert_eq!(4, t0.ready_at()); // 2 + ceil(8/4)=4
        assert_eq!(6, t1.ready_at()); // launches two cycles later while req0 still pending
        assert_eq!(8, t2.ready_at());
    }

    #[test]
    // Verify that completions_per_cycle limits how many results can be surfaced in a single cycle,
    // even if multiple requests have already finished processing
    fn completion_bandwidth_limits_visible_results() {
        let mut server = make_server(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            completions_per_cycle: 1,
            ..ServerConfig::default()
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();
        server
            .try_enqueue(0, ServiceRequest::new("req1", 4))
            .unwrap();

        // Both requests mature by cycle 1, but only one completion can be surfaced per cycle
        assert!(server.pop_ready(1).is_some());
        // Second completion becomes visible on the next cycle
        assert!(server.pop_ready(1).is_none());
        assert!(server.pop_ready(2).is_some());
    }

    #[test]
    // Ensure that ServerStats correctly tracks request issuance, completion,
    // byte counts, and maximum outstanding queue depth
    fn server_stats_track_activity() {
        let mut server = make_server(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        });

        server
            .try_enqueue(0, ServiceRequest::new("req0", 4))
            .unwrap();
        server
            .try_enqueue(1, ServiceRequest::new("req1", 8))
            .unwrap();

        server.pop_ready(2);
        server.pop_ready(4);

        let stats = server.stats();
        assert_eq!(2, stats.issued);
        assert_eq!(2, stats.completed);
        assert_eq!(12, stats.bytes_issued);
        assert_eq!(12, stats.bytes_completed);
        assert_eq!(2, stats.max_outstanding);
    }
}
