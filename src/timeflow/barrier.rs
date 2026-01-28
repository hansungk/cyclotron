use serde::Deserialize;

use crate::timeq::{Cycle, ServerConfig, ServiceRequest, TimedServer};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BarrierConfig {
    pub enabled: bool,
    pub expected_warps: usize,
    #[serde(flatten)]
    pub queue: ServerConfig,
}

impl Default for BarrierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            expected_warps: 0,
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
    server: TimedServer<()>,
    arrived: Vec<bool>,
    releasing: Vec<usize>,
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
            server: TimedServer::new(config.queue),
            arrived: vec![false; num_warps.max(1)],
            releasing: Vec::new(),
        }
    }

    pub fn arrive(&mut self, now: Cycle, warp: usize) -> Option<Cycle> {
        if !self.enabled {
            return Some(now);
        }
        if self.expected_warps == 0 || warp >= self.arrived.len() {
            return Some(now);
        }

        if !self.releasing.is_empty() {
            return self
                .server
                .oldest_ticket()
                .map(|ticket| ticket.ready_at())
                .or_else(|| Some(now));
        }

        self.arrived[warp] = true;
        let arrived_count = self.arrived.iter().filter(|&&v| v).count();
        if arrived_count < self.expected_warps {
            return None;
        }

        let request = ServiceRequest::new((), 0);
        if let Ok(ticket) = self.server.try_enqueue(now, request) {
            let mut warps = Vec::new();
            for (idx, arrived) in self.arrived.iter_mut().enumerate() {
                if *arrived {
                    warps.push(idx);
                    *arrived = false;
                }
            }
            self.releasing = warps;
            Some(ticket.ready_at())
        } else {
            None
        }
    }

    pub fn tick(&mut self, now: Cycle) -> Option<Vec<usize>> {
        if !self.enabled {
            return None;
        }
        let mut released: Option<Vec<usize>> = None;
        self.server.service_ready(now, |_| {
            if !self.releasing.is_empty() {
                released = Some(std::mem::take(&mut self.releasing));
            }
        });
        released
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
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
        assert!(barrier.arrive(0, 0).is_none());
        let release_at = barrier.arrive(0, 1).expect("barrier should schedule");
        assert!(release_at >= 2);

        assert!(barrier.tick(1).is_none());
        let released = barrier.tick(release_at).expect("barrier should release");
        assert_eq!(released.len(), 2);
    }
}
