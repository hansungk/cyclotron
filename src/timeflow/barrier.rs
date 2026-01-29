use serde::Deserialize;

use crate::timeq::{Cycle, ServerConfig, ServiceRequest, TimedServer};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BarrierConfig {
    pub enabled: bool,
    pub expected_warps: usize,
    pub barrier_id_bits: u32,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for BarrierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            expected_warps: 0,
            barrier_id_bits: 0,
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

pub struct BarrierManager {
    enabled: bool,
    expected_warps: usize,
    barrier_id_bits: u32,
    server: TimedServer<u32>,
    states: std::collections::HashMap<u32, BarrierState>,
    num_warps: usize,
}

struct BarrierState {
    arrived: Vec<bool>,
    releasing: Vec<usize>,
    release_at: Option<Cycle>,
}

impl BarrierManager {
    pub fn new(config: BarrierConfig, num_warps: usize) -> Self {
        let expected = if config.expected_warps == 0 {
            num_warps.max(1)
        } else {
            config.expected_warps.min(num_warps.max(1))
        };
        Self {
            enabled: config.enabled,
            expected_warps: expected,
            barrier_id_bits: config.barrier_id_bits,
            server: TimedServer::new(config.queue),
            states: std::collections::HashMap::new(),
            num_warps: num_warps.max(1),
        }
    }

    pub fn arrive(&mut self, now: Cycle, warp: usize, barrier_id: u32) -> Option<Cycle> {
        if !self.enabled {
            return Some(now);
        }
        if self.expected_warps == 0 || warp >= self.num_warps {
            return Some(now);
        }

        let id = self.normalize_id(barrier_id);
        let state = self.states.entry(id).or_insert_with(|| BarrierState {
            arrived: vec![false; self.num_warps],
            releasing: Vec::new(),
            release_at: None,
        });

        if let Some(release_at) = state.release_at {
            return Some(release_at);
        }

        state.arrived[warp] = true;
        let arrived_count = state.arrived.iter().filter(|&&v| v).count();
        if arrived_count < self.expected_warps {
            return None;
        }

        let request = ServiceRequest::new(id, 0);
        if let Ok(ticket) = self.server.try_enqueue(now, request) {
            let mut warps = Vec::new();
            for (idx, arrived) in state.arrived.iter_mut().enumerate() {
                if *arrived {
                    warps.push(idx);
                    *arrived = false;
                }
            }
            state.releasing = warps;
            state.release_at = Some(ticket.ready_at());
            Some(ticket.ready_at())
        } else {
            None
        }
    }

    pub fn tick(&mut self, now: Cycle) -> Option<Vec<usize>> {
        if !self.enabled {
            return None;
        }
        let mut released = Vec::new();
        self.server.service_ready(now, |result| {
            let barrier_id = result.payload;
            if let Some(state) = self.states.get_mut(&barrier_id) {
                if !state.releasing.is_empty() {
                    released.extend(state.releasing.drain(..));
                }
                state.release_at = None;
            }
        });

        if released.is_empty() {
            None
        } else {
            Some(released)
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn normalize_id(&self, barrier_id: u32) -> u32 {
        if self.barrier_id_bits == 0 {
            barrier_id
        } else if self.barrier_id_bits >= 32 {
            barrier_id
        } else {
            let mask = (1u32 << self.barrier_id_bits) - 1;
            barrier_id & mask
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BarrierConfig, BarrierManager};

    #[test]
    fn barrier_releases_after_all_warps_arrive() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 2;
        cfg.queue.base_latency = 2;

        let mut barrier = BarrierManager::new(cfg, 2);
        assert!(barrier.arrive(0, 0, 0).is_none());
        let release_at = barrier.arrive(0, 1, 0).expect("barrier should schedule");
        assert!(release_at >= 2);

        assert!(barrier.tick(1).is_none());
        let released = barrier.tick(release_at).expect("barrier should release");
        assert_eq!(released.len(), 2);
    }

    #[test]
    fn barrier_tracks_multiple_ids() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 2;
        cfg.queue.base_latency = 1;

        let mut barrier = BarrierManager::new(cfg, 2);
        assert!(barrier.arrive(0, 0, 1).is_none());
        assert!(barrier.arrive(0, 1, 2).is_none());

        let rel0 = barrier.arrive(0, 1, 1).expect("barrier 1 should schedule");
        let rel1 = barrier.arrive(0, 0, 2).expect("barrier 2 should schedule");
        let cycle = rel0.min(rel1);
        let released = barrier.tick(cycle).unwrap_or_default();
        assert!(!released.is_empty(), "expected some warps released");
    }

    #[test]
    fn single_warp_barrier_immediate() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 1;
        cfg.queue.base_latency = 0;

        let mut barrier = BarrierManager::new(cfg, 1);
        let release_at = barrier.arrive(0, 0, 0).expect("should schedule release");
        let released = barrier.tick(release_at).expect("should release");
        assert_eq!(released, vec![0]);
    }

    #[test]
    fn partial_arrival_waits() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 4;
        cfg.queue.base_latency = 1;

        let mut barrier = BarrierManager::new(cfg, 4);
        assert!(barrier.arrive(0, 0, 0).is_none());
        assert!(barrier.arrive(0, 1, 0).is_none());
        assert!(barrier.tick(2).is_none());
    }

    #[test]
    fn disabled_barrier_passthrough() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = false;
        cfg.expected_warps = 4;

        let mut barrier = BarrierManager::new(cfg, 4);
        let release = barrier.arrive(0, 0, 0);
        assert_eq!(release, Some(0));
    }

    #[test]
    fn warp_arrives_twice_same_barrier() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 2;
        cfg.queue.base_latency = 1;

        let mut barrier = BarrierManager::new(cfg, 2);
        assert!(barrier.arrive(0, 0, 0).is_none());
        assert!(barrier.arrive(0, 0, 0).is_none());
        let release_at = barrier.arrive(0, 1, 0).expect("should schedule");
        let released = barrier.tick(release_at).expect("release expected");
        assert_eq!(released.len(), 2);
    }

    #[test]
    fn expected_warps_exceeds_num_warps() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 8;
        cfg.queue.base_latency = 0;

        let mut barrier = BarrierManager::new(cfg, 4);
        for warp in 0..4 {
            let maybe = barrier.arrive(0, warp, 0);
            if warp < 3 {
                assert!(maybe.is_none());
            } else {
                assert!(maybe.is_some());
            }
        }
    }

    #[test]
    fn rapid_barrier_cycles() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 2;
        cfg.queue.base_latency = 0;

        let mut barrier = BarrierManager::new(cfg, 2);
        for cycle in 0..100 {
            assert!(barrier.arrive(cycle, 0, 0).is_none());
            let release_at = barrier.arrive(cycle, 1, 0).expect("schedule");
            let released = barrier.tick(release_at).expect("release");
            assert_eq!(released.len(), 2);
        }
    }

    #[test]
    fn barrier_with_different_ids_interleaved() {
        let mut cfg = BarrierConfig::default();
        cfg.enabled = true;
        cfg.expected_warps = 2;
        cfg.queue.base_latency = 0;

        let mut barrier = BarrierManager::new(cfg, 2);
        assert!(barrier.arrive(0, 0, 1).is_none());
        assert!(barrier.arrive(0, 0, 2).is_none());

        let rel1 = barrier.arrive(0, 1, 1).expect("barrier 1 schedule");
        let rel2 = barrier.arrive(0, 1, 2).expect("barrier 2 schedule");
        let mut released = Vec::new();
        if let Some(mut warps) = barrier.tick(rel1) {
            released.append(&mut warps);
        }
        if let Some(mut warps) = barrier.tick(rel2.max(rel1.saturating_add(1))) {
            released.append(&mut warps);
        }
        assert_eq!(released.len(), 4);
    }
}
