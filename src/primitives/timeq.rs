use crate::base::module::IsModule;
use serde::Deserialize;
use std::collections::VecDeque;

pub type Cycle = u64;

// Make sure a retry cycle is at least now + 1
pub fn normalize_retry(now: Cycle, suggested: Cycle) -> Cycle {
    suggested.max(now.saturating_add(1))
}

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
    pub(crate) fn new(issued_at: Cycle, ready_at: Cycle, size_bytes: u32) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptStatus {
    Ready,
    QueueFull { capacity: usize },
    Busy { available_at: Cycle },
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
        match self.accept_status(now) {
            AcceptStatus::Ready => {}
            AcceptStatus::QueueFull { capacity } => {
                self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
                return Err(Backpressure::QueueFull { request, capacity });
            }
            AcceptStatus::Busy { available_at } => {
                self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                return Err(Backpressure::Busy {
                    request,
                    available_at,
                });
            }
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

    pub fn accept_status(&self, now: Cycle) -> AcceptStatus {
        if self.outstanding_len() >= self.config.queue_capacity {
            return AcceptStatus::QueueFull {
                capacity: self.config.queue_capacity,
            };
        }

        if self.outstanding_len() == 0 && now < self.warmup_until {
            return AcceptStatus::Busy {
                available_at: self.warmup_until,
            };
        }

        AcceptStatus::Ready
    }

    pub fn available_slots(&self) -> usize {
        self.config
            .queue_capacity
            .saturating_sub(self.outstanding_len())
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
#[path = "tests/timeq.rs"]
mod tests;
