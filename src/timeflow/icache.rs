use serde::Deserialize;

use crate::timeflow::simple_queue::SimpleTimedQueue;
use crate::timeflow::types::RejectReason;
pub use crate::timeflow::types::RejectReason as IcacheRejectReason;
use crate::timeq::{Cycle, ServerConfig, Ticket};

#[derive(Debug, Clone, Copy, Default)]
pub struct IcacheStats {
    pub issued: u64,
    pub completed: u64,
    pub hits: u64,
    pub misses: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
    pub bytes_issued: u64,
    pub bytes_completed: u64,
    pub last_completion_cycle: Option<Cycle>,
}

// Alias to central RejectReason for Icache.

#[derive(Debug, Clone)]
pub struct IcacheRequest {
    pub id: u64,
    pub core_id: usize,
    pub warp: usize,
    pub pc: u64,
    pub line_addr: u64,
    pub bytes: u32,
    pub miss: bool,
}

impl IcacheRequest {
    pub fn new(warp: usize, pc: u32, bytes: u32) -> Self {
        Self {
            id: 0,
            core_id: 0,
            warp,
            pc: pc as u64,
            line_addr: 0,
            bytes,
            miss: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IcacheIssue {
    pub ticket: Ticket,
}

pub type IcacheReject = crate::timeflow::types::RejectWith<IcacheRequest>;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct IcachePolicyConfig {
    pub hit_rate: f64,
    pub line_bytes: u32,
    pub seed: u64,
}

impl Default for IcachePolicyConfig {
    fn default() -> Self {
        Self {
            hit_rate: 0.98,
            line_bytes: 32,
            seed: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IcacheFlowConfig {
    pub hit: ServerConfig,
    pub miss: ServerConfig,
    pub policy: IcachePolicyConfig,
}

impl Default for IcacheFlowConfig {
    fn default() -> Self {
        Self {
            hit: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 1,
                queue_capacity: 16,
                completions_per_cycle: u32::MAX,
                warmup_latency: 0,
            },
            miss: ServerConfig {
                base_latency: 40,
                bytes_per_cycle: 1,
                queue_capacity: 8,
                completions_per_cycle: u32::MAX,
                warmup_latency: 0,
            },
            policy: IcachePolicyConfig::default(),
        }
    }
}

pub struct IcacheSubgraph {
    hit: SimpleTimedQueue<IcacheRequest>,
    miss: SimpleTimedQueue<IcacheRequest>,
    policy: IcachePolicyConfig,
    stats: IcacheStats,
}

impl IcacheSubgraph {
    pub fn new(config: IcacheFlowConfig) -> Self {
        Self {
            hit: SimpleTimedQueue::new(true, config.hit),
            miss: SimpleTimedQueue::new(true, config.miss),
            policy: config.policy,
            stats: IcacheStats::default(),
        }
    }

    pub fn issue(
        &mut self,
        now: Cycle,
        mut request: IcacheRequest,
    ) -> Result<IcacheIssue, IcacheReject> {
        let line_bytes = self.policy.line_bytes.max(1);
        request.line_addr = line_addr(request.pc, line_bytes);
        let hit = decide(self.policy.hit_rate, request.line_addr ^ self.policy.seed);
        request.miss = !hit;

        let bytes = request.bytes;
        let queue = if hit { &mut self.hit } else { &mut self.miss };

        match queue.try_issue_with_payload(now, request, 0) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                self.stats.bytes_issued = self.stats.bytes_issued.saturating_add(bytes as u64);
                if hit {
                    self.stats.hits = self.stats.hits.saturating_add(1);
                } else {
                    self.stats.misses = self.stats.misses.saturating_add(1);
                }
                Ok(IcacheIssue { ticket })
            }
            Err(err) => {
                match err.reason {
                    RejectReason::Busy => {
                        self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                    }
                    RejectReason::QueueFull => {
                        self.stats.queue_full_rejects =
                            self.stats.queue_full_rejects.saturating_add(1);
                    }
                }
                Err(IcacheReject {
                    payload: err.payload,
                    retry_at: err.retry_at,
                    reason: err.reason,
                })
            }
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.hit.tick(now, |request: IcacheRequest| {
            self.stats.completed = self.stats.completed.saturating_add(1);
            self.stats.bytes_completed = self
                .stats
                .bytes_completed
                .saturating_add(request.bytes as u64);
            self.stats.last_completion_cycle = Some(now);
        });
        self.miss.tick(now, |request: IcacheRequest| {
            self.stats.completed = self.stats.completed.saturating_add(1);
            self.stats.bytes_completed = self
                .stats
                .bytes_completed
                .saturating_add(request.bytes as u64);
            self.stats.last_completion_cycle = Some(now);
        });
    }

    pub fn stats(&self) -> IcacheStats {
        self.stats
    }
}

fn line_addr(addr: u64, line_bytes: u32) -> u64 {
    let bytes = line_bytes.max(1) as u64;
    addr / bytes
}

fn decide(rate: f64, key: u64) -> bool {
    let clamped = rate.clamp(0.0, 1.0);
    if clamped <= 0.0 {
        return false;
    }
    if clamped >= 1.0 {
        return true;
    }
    let threshold = (clamped * (u64::MAX as f64)) as u64;
    hash_u64(key) <= threshold
}

fn hash_u64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icache_hit_ready_at_now_with_zero_latency() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 1.0;
        cfg.hit.base_latency = 0;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x1000, 8);
        let issue = icache.issue(10, req).expect("hit should accept");
        assert_eq!(issue.ticket.ready_at(), 10);
    }

    #[test]
    fn icache_miss_latency_applies() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 0.0;
        cfg.miss.base_latency = 7;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x1000, 8);
        let issue = icache.issue(5, req).expect("miss should accept");
        assert_eq!(issue.ticket.ready_at(), 12);
    }

    #[test]
    fn icache_queue_full_rejects() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 0.0;
        cfg.miss.queue_capacity = 1;
        let mut icache = IcacheSubgraph::new(cfg);
        let req0 = IcacheRequest::new(0, 0x1000, 8);
        icache.issue(0, req0).expect("first miss should accept");
        let req1 = IcacheRequest::new(1, 0x2000, 8);
        let err = icache
            .issue(0, req1)
            .expect_err("second miss should reject");
        assert!(err.retry_at > 0);
    }

    #[test]
    fn icache_hit_rate_one_always_hits() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 1.0;
        cfg.hit.base_latency = 1;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x2000, 8);
        let issue = icache.issue(10, req).expect("hit should accept");
        assert_eq!(11, issue.ticket.ready_at());
    }

    #[test]
    fn icache_hit_rate_zero_always_misses() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 0.0;
        cfg.miss.base_latency = 5;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x2000, 8);
        let issue = icache.issue(3, req).expect("miss should accept");
        assert_eq!(8, issue.ticket.ready_at());
    }

    #[test]
    fn icache_line_address_computed_correctly() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 1.0;
        cfg.policy.line_bytes = 32;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x1040, 8);
        let issue = icache.issue(0, req).expect("hit should accept");
        icache.tick(issue.ticket.ready_at());
        let stats = icache.stats();
        assert_eq!(1, stats.issued);
        assert_eq!(1, stats.hits);
    }

    #[test]
    fn icache_stats_track_hits_and_misses() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 0.0;
        let mut icache = IcacheSubgraph::new(cfg);
        let req = IcacheRequest::new(0, 0x1000, 8);
        let issue = icache.issue(0, req).expect("miss should accept");
        icache.tick(issue.ticket.ready_at());
        let stats = icache.stats();
        assert_eq!(1, stats.issued);
        assert_eq!(1, stats.misses);
        assert_eq!(1, stats.completed);
    }

    #[test]
    fn icache_many_parallel_fetches() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 1.0;
        cfg.hit.queue_capacity = 8;
        cfg.hit.base_latency = 0;
        let mut icache = IcacheSubgraph::new(cfg);

        for warp in 0..8 {
            let req = IcacheRequest::new(warp, 0x1000 + (warp as u32) * 4, 8);
            icache.issue(0, req).expect("issue should succeed");
        }
        icache.tick(0);
        let stats = icache.stats();
        assert_eq!(8, stats.issued);
        assert_eq!(8, stats.completed);
    }

    #[test]
    fn icache_pc_zero_handling() {
        let mut cfg = IcacheFlowConfig::default();
        cfg.policy.hit_rate = 1.0;
        cfg.hit.base_latency = 0;
        let mut icache = IcacheSubgraph::new(cfg);

        let req = IcacheRequest::new(0, 0, 8);
        let issue = icache.issue(0, req).expect("issue should succeed");
        icache.tick(issue.ticket.ready_at());
        let stats = icache.stats();
        assert_eq!(1, stats.completed);
    }
}
