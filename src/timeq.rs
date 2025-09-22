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
        Self { payload, size_bytes }
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
    QueueFull { request: ServiceRequest<T>, capacity: usize },
    // The server is currently busy
    Busy { request: ServiceRequest<T>, available_at: Cycle },
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
