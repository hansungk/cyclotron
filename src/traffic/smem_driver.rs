use crate::traffic::config::TrafficConfig;
use crate::traffic::logging::TrafficLogger;
use crate::traffic::patterns::PatternEngine;
use crate::{
    base::behavior::ModuleBehaviors,
    muon::{core::MuonCore, gmem::CoreTimingModel},
    timeflow::SmemRequest,
    timeq::Cycle,
};

#[derive(Debug, Clone, Default)]
struct LaneState {
    pattern_idx: usize,
    next_req: u32,
    inflight: usize,
    retry_at: Cycle,
}

#[derive(Debug, Clone, Default)]
struct CoreState {
    lanes: Vec<LaneState>,
    done: bool,
    done_logged: bool,
}

#[derive(Debug)]
pub struct SmemTrafficDriver {
    config: TrafficConfig,
    done: bool,
    pub pattern_engine: PatternEngine,
    core_states: Vec<CoreState>,
}

impl SmemTrafficDriver {
    pub fn new(config: &TrafficConfig) -> Self {
        Self {
            config: config.clone(),
            done: !config.enabled,
            pattern_engine: PatternEngine::new(config),
            core_states: Vec::new(),
        }
    }

    pub fn tick(&mut self, cores: &mut [MuonCore]) {
        if self.done {
            return;
        }
        if self.pattern_engine.is_empty() {
            self.done = true;
            return;
        }
        self.ensure_core_state(cores);

        let reqs_per_pattern = self.config.reqs_per_pattern;
        let max_inflight = self.config.issue.max_inflight_per_lane.max(1);
        let retry_backoff = self.config.issue.retry_backoff_min.max(1);
        let lockstep = self.config.lockstep_patterns;
        let print_lines = self.config.logging.print_traffic_lines;

        for (core_id, core) in cores.iter_mut().enumerate() {
            if self
                .core_states
                .get(core_id)
                .map(|s| s.done)
                .unwrap_or(true)
            {
                core.scheduler.tick_one();
                continue;
            }

            let mut finished_pattern: Option<(usize, Cycle)> = None;
            let mut core_finished_this_tick = false;
            let state = self
                .core_states
                .get_mut(core_id)
                .expect("missing traffic driver state for core");
            let model_present = core.with_timing_model(|timing, scheduler, now| {
                timing.tick(now, scheduler);
                for completion in timing.drain_smem_completion_events() {
                    if let Some(lane) = state.lanes.get_mut(completion.warp) {
                        lane.inflight = lane.inflight.saturating_sub(1);
                    }
                }

                let completed_pattern = if lockstep {
                    Self::drive_lockstep(
                        state,
                        timing,
                        now,
                        &self.pattern_engine,
                        reqs_per_pattern,
                        max_inflight,
                        retry_backoff,
                    )
                } else {
                    Self::drive_independent(
                        state,
                        timing,
                        now,
                        &self.pattern_engine,
                        reqs_per_pattern,
                        max_inflight,
                        retry_backoff,
                    )
                };
                if let Some(idx) = completed_pattern {
                    finished_pattern = Some((idx, now));
                }
                if state.done {
                    core_finished_this_tick = true;
                }
            });
            if model_present.is_none() {
                panic!("frontend_mode=traffic_smem requires sim.timing=true");
            }

            core.scheduler.tick_one();

            if let Some((pattern_idx, finished_cycle)) = finished_pattern {
                if print_lines {
                    if let Some(pattern_name) = self.pattern_engine.pattern_name(pattern_idx) {
                        TrafficLogger::log_pattern_checkpoint(
                            core_id,
                            pattern_name,
                            finished_cycle,
                        );
                    }
                }
            }

            if core_finished_this_tick && !state.done_logged {
                state.done_logged = true;
                if print_lines {
                    TrafficLogger::log_core_done(core_id);
                }
            }
        }

        self.done = self.core_states.iter().all(|state| state.done);
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn ensure_core_state(&mut self, cores: &[MuonCore]) {
        while self.core_states.len() < cores.len() {
            let core_id = self.core_states.len();
            let lane_cap = cores[core_id].warps.len();
            assert!(
                self.config.num_lanes <= lane_cap,
                "traffic.num_lanes={} exceeds core {} warp capacity {}. Set muon.num_warps >= traffic.num_lanes for traffic_smem mode.",
                self.config.num_lanes,
                core_id,
                lane_cap
            );
            let lanes = vec![LaneState::default(); self.config.num_lanes];
            self.core_states.push(CoreState {
                lanes,
                done: false,
                done_logged: false,
            });
        }
    }

    fn drive_lockstep(
        state: &mut CoreState,
        timing: &mut CoreTimingModel,
        now: Cycle,
        patterns: &PatternEngine,
        reqs_per_pattern: u32,
        max_inflight: usize,
        retry_backoff: Cycle,
    ) -> Option<usize> {
        if state.done || state.lanes.is_empty() {
            state.done = true;
            return None;
        }

        let pattern_idx = state.lanes[0].pattern_idx;
        if pattern_idx >= patterns.len() {
            state.done = state.lanes.iter().all(|lane| lane.inflight == 0);
            return None;
        }

        let pattern = patterns
            .pattern(pattern_idx)
            .expect("pattern index out of range in lockstep driver");

        for lane_idx in 0..state.lanes.len() {
            let lane = &mut state.lanes[lane_idx];
            if lane.pattern_idx != pattern_idx {
                continue;
            }
            if lane.next_req >= reqs_per_pattern {
                continue;
            }
            if lane.inflight >= max_inflight {
                continue;
            }
            if lane.retry_at > now {
                continue;
            }

            let addr = patterns
                .lane_addr(pattern_idx, lane.next_req, lane_idx)
                .expect("missing lane address for pattern");
            let request = SmemRequest {
                id: 0,
                warp: lane_idx,
                addr,
                lane_addrs: None,
                bytes: pattern.req_bytes,
                active_lanes: 1,
                is_store: pattern.op.is_store(),
                bank: 0,
                subbank: 0,
            };

            match timing.issue_smem_request_frontend(now, request) {
                Ok(_) => {
                    lane.next_req = lane.next_req.saturating_add(1);
                    lane.inflight = lane.inflight.saturating_add(1);
                    lane.retry_at = now.saturating_add(1);
                }
                Err(retry_at) => {
                    lane.retry_at = retry_at.max(now.saturating_add(retry_backoff));
                }
            }
        }

        let pattern_done = state
            .lanes
            .iter()
            .all(|lane| lane.pattern_idx > pattern_idx || lane.next_req >= reqs_per_pattern)
            && state
                .lanes
                .iter()
                .all(|lane| lane.pattern_idx > pattern_idx || lane.inflight == 0);
        if pattern_done {
            for lane in &mut state.lanes {
                if lane.pattern_idx == pattern_idx {
                    lane.pattern_idx = lane.pattern_idx.saturating_add(1);
                    lane.next_req = 0;
                    lane.retry_at = now;
                }
            }
            if state
                .lanes
                .iter()
                .all(|lane| lane.pattern_idx >= patterns.len() && lane.inflight == 0)
            {
                state.done = true;
            }
            return Some(pattern_idx);
        }

        None
    }

    fn drive_independent(
        state: &mut CoreState,
        timing: &mut CoreTimingModel,
        now: Cycle,
        patterns: &PatternEngine,
        reqs_per_pattern: u32,
        max_inflight: usize,
        retry_backoff: Cycle,
    ) -> Option<usize> {
        if state.done || state.lanes.is_empty() {
            state.done = true;
            return None;
        }

        for lane_idx in 0..state.lanes.len() {
            loop {
                let lane = &mut state.lanes[lane_idx];
                if lane.pattern_idx >= patterns.len() {
                    break;
                }
                if lane.next_req >= reqs_per_pattern && lane.inflight == 0 {
                    lane.pattern_idx = lane.pattern_idx.saturating_add(1);
                    lane.next_req = 0;
                    lane.retry_at = now;
                    continue;
                }
                break;
            }

            let lane = &mut state.lanes[lane_idx];
            if lane.pattern_idx >= patterns.len() {
                continue;
            }
            if lane.next_req >= reqs_per_pattern {
                continue;
            }
            if lane.inflight >= max_inflight {
                continue;
            }
            if lane.retry_at > now {
                continue;
            }

            let pattern = patterns
                .pattern(lane.pattern_idx)
                .expect("pattern index out of range in independent driver");
            let addr = patterns
                .lane_addr(lane.pattern_idx, lane.next_req, lane_idx)
                .expect("missing lane address for pattern");
            let request = SmemRequest {
                id: 0,
                warp: lane_idx,
                addr,
                lane_addrs: None,
                bytes: pattern.req_bytes,
                active_lanes: 1,
                is_store: pattern.op.is_store(),
                bank: 0,
                subbank: 0,
            };
            match timing.issue_smem_request_frontend(now, request) {
                Ok(_) => {
                    lane.next_req = lane.next_req.saturating_add(1);
                    lane.inflight = lane.inflight.saturating_add(1);
                    lane.retry_at = now.saturating_add(1);
                }
                Err(retry_at) => {
                    lane.retry_at = retry_at.max(now.saturating_add(retry_backoff));
                }
            }
        }

        state.done = state
            .lanes
            .iter()
            .all(|lane| lane.pattern_idx >= patterns.len() && lane.inflight == 0);
        None
    }
}
