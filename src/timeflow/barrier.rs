use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::timeq::{Cycle, ServerConfig, ServiceRequest, TimedServer};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BarrierConfig {
    pub enabled: bool,
    pub expected_warps: Option<usize>,
    pub barrier_id_bits: u32,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for BarrierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            expected_warps: None,
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
    server: TimedServer<u32>,
    states: HashMap<u32, BarrierState>,
    num_warps: usize,

    id_mask: Option<u32>,
    stats: BarrierSummary,
}

struct BarrierState {
    arrived: Vec<bool>,
    releasing: Vec<usize>,
    release_at: Option<Cycle>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BarrierSummary {
    pub arrivals: u64,
    pub queue_rejects: u64,
    pub release_events: u64,
    pub warps_released: u64,
    pub max_release_batch: u64,
    pub total_scheduled_wait_cycles: u64,
    pub max_scheduled_wait_cycles: u64,
}

impl BarrierManager {
    pub fn new(config: BarrierConfig, num_warps: usize) -> Self {
        let num_warps = num_warps.max(1);
        let expected = match config.expected_warps {
            None => num_warps,
            Some(v) => v.min(num_warps),
        };
        let id_mask = if config.barrier_id_bits == 0 {
            None
        } else if config.barrier_id_bits >= 32 {
            None
        } else {
            Some((1u32 << config.barrier_id_bits) - 1)
        };
        let state_capacity = if config.barrier_id_bits == 0 {
            1
        } else {
            (1usize << config.barrier_id_bits.min(16)).max(1)
        };
        Self {
            enabled: config.enabled,
            expected_warps: expected,
            server: TimedServer::new(config.queue),
            states: HashMap::with_capacity(state_capacity),
            num_warps,

            id_mask,
            stats: BarrierSummary::default(),
        }
    }

    pub fn arrive(&mut self, now: Cycle, warp: usize, barrier_id: u32) -> Option<Cycle> {
        if !self.enabled {
            return Some(now);
        }
        if warp >= self.num_warps {
            return Some(now);
        }
        self.stats.arrivals = self.stats.arrivals.saturating_add(1);

        let id = self.apply_id_mask(barrier_id);
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
        let ready = arrived_count >= self.expected_warps;

        if !ready {
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
            let wait_cycles = ticket.ready_at().saturating_sub(now);
            self.stats.total_scheduled_wait_cycles = self
                .stats
                .total_scheduled_wait_cycles
                .saturating_add(wait_cycles);
            self.stats.max_scheduled_wait_cycles =
                self.stats.max_scheduled_wait_cycles.max(wait_cycles);
            Some(ticket.ready_at())
        } else {
            self.stats.queue_rejects = self.stats.queue_rejects.saturating_add(1);
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
            self.stats.release_events = self.stats.release_events.saturating_add(1);
            self.stats.warps_released = self
                .stats
                .warps_released
                .saturating_add(released.len() as u64);
            self.stats.max_release_batch = self
                .stats
                .max_release_batch
                .max(released.len() as u64);
            Some(released)
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn apply_id_mask(&self, barrier_id: u32) -> u32 {
        self.id_mask.map_or(barrier_id, |mask| barrier_id & mask)
    }

    pub fn stats(&self) -> BarrierSummary {
        self.stats
    }

    pub fn clear_stats(&mut self) {
        self.stats = BarrierSummary::default();
    }
}
