use std::collections::HashMap;

use serde_json::json;

use crate::traffic::config::TrafficConfig;
use crate::traffic::logging::{PatternCheckpoint, TrafficLogger};
use crate::traffic::patterns::PatternEngine;
use crate::{
    base::behavior::ModuleBehaviors,
    muon::{core::MuonCore, gmem::CoreTimingModel},
    timeflow::SmemRequest,
    timeq::Cycle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LanePhase {
    ActiveIssue,
    WaitRetry,
    Drain,
    BarrierWait,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierPhase {
    Running,
    WaitingDrain,
    Advance,
    Done,
}

#[derive(Debug, Clone)]
struct LaneState {
    lane_id: usize,
    source_warp: usize,
    phase: LanePhase,
    current_pattern_idx: usize,
    issued_in_pattern: u32,
    completed_in_pattern: u32,
    inflight: usize,
    next_t: u32,
    retry_at: Cycle,
    last_request_id: Option<u64>,
}

impl LaneState {
    fn new(lane_id: usize) -> Self {
        Self {
            lane_id,
            source_warp: lane_id,
            phase: LanePhase::ActiveIssue,
            current_pattern_idx: 0,
            issued_in_pattern: 0,
            completed_in_pattern: 0,
            inflight: 0,
            next_t: 0,
            retry_at: 0,
            last_request_id: None,
        }
    }

    fn reset_for_pattern(&mut self, pattern_idx: usize, now: Cycle) {
        self.current_pattern_idx = pattern_idx;
        self.issued_in_pattern = 0;
        self.completed_in_pattern = 0;
        self.next_t = 0;
        self.retry_at = now;
        self.last_request_id = None;
        if self.inflight == 0 {
            self.phase = LanePhase::ActiveIssue;
        } else {
            self.phase = LanePhase::Drain;
        }
    }
}

#[derive(Debug, Clone)]
struct PatternBarrierState {
    current_pattern_idx: usize,
    phase: BarrierPhase,
    pattern_start_cycle: Cycle,
}

#[derive(Debug, Clone)]
struct CoreState {
    core_id: usize,
    lanes: Vec<LaneState>,
    barrier: PatternBarrierState,
    checkpoints: Vec<PatternCheckpoint>,
    completion_route: HashMap<u64, usize>, // request_id -> lane_id
    next_request_id: u64,
    done_logged: bool,
}

impl CoreState {
    fn is_done(&self) -> bool {
        self.barrier.phase == BarrierPhase::Done
            && self.completion_route.is_empty()
            && self.lanes.iter().all(|lane| lane.inflight == 0)
    }
}

#[derive(Debug)]
pub struct SmemTrafficDriver {
    config: TrafficConfig,
    done: bool,
    results_emitted: bool,
    pub pattern_engine: PatternEngine,
    core_states: Vec<CoreState>,
}

impl SmemTrafficDriver {
    pub fn new(config: &TrafficConfig) -> Self {
        Self {
            config: config.clone(),
            done: !config.enabled,
            results_emitted: false,
            pattern_engine: PatternEngine::new(config),
            core_states: Vec::new(),
        }
    }

    pub fn tick_core(&mut self, core_id: usize, core: &mut MuonCore) {
        if self.done {
            core.scheduler.tick_one();
            return;
        }
        if self.pattern_engine.is_empty() {
            self.done = true;
            self.maybe_emit_results();
            core.scheduler.tick_one();
            return;
        }

        self.ensure_core_state_for(core_id, core);

        let reqs_per_pattern = self.config.reqs_per_pattern;
        let max_inflight = self.config.issue.max_inflight_per_lane.max(1);
        let retry_backoff = self.config.issue.retry_backoff_min.max(1);
        let lockstep = self.config.lockstep_patterns;
        let print_lines = self.config.logging.print_traffic_lines;
        let patterns = &self.pattern_engine;

        let mut finished_checkpoint: Option<PatternCheckpoint> = None;
        let mut core_just_done = false;
        let mut model_none = false;
        {
            let state = self
                .core_states
                .get_mut(core_id)
                .expect("missing traffic driver state for core");

            if !state.is_done() {
                let model_present = core.with_timing_model(|timing, scheduler, now| {
                    timing.tick(now, scheduler);
                    let events = timing.drain_smem_completion_events();
                    Self::route_completions(state, &events);

                    finished_checkpoint = if lockstep {
                        Self::tick_core_lockstep(
                            state,
                            timing,
                            now,
                            patterns,
                            reqs_per_pattern,
                            max_inflight,
                            retry_backoff,
                        )
                    } else {
                        Self::tick_core_independent(
                            state,
                            timing,
                            now,
                            patterns,
                            reqs_per_pattern,
                            max_inflight,
                            retry_backoff,
                        )
                    };
                    core_just_done = state.is_done();
                });
                model_none = model_present.is_none();
            }
        }

        if model_none {
            panic!("frontend_mode=traffic_smem requires sim.timing=true");
        }

        core.scheduler.tick_one();

        if let Some(checkpoint) = finished_checkpoint {
            if print_lines {
                TrafficLogger::log_pattern_checkpoint(
                    checkpoint.core_id,
                    &checkpoint.pattern_name,
                    checkpoint.finished_cycle,
                );
            }
        }

        if let Some(state) = self.core_states.get_mut(core_id) {
            if core_just_done && !state.done_logged {
                state.done_logged = true;
                if print_lines {
                    TrafficLogger::log_core_done(core_id);
                }
            }
        }

        self.done = self.core_states.iter().all(CoreState::is_done);
        self.maybe_emit_results();
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn ensure_core_state_for(&mut self, core_id: usize, core: &MuonCore) {
        while self.core_states.len() <= core_id {
            let new_core_id = self.core_states.len();
            let lane_cap = if new_core_id == core_id {
                core.warps.len()
            } else {
                self.config.num_lanes
            };
            assert!(
                self.config.num_lanes <= lane_cap,
                "traffic.num_lanes={} exceeds core {} warp capacity {}. Set muon.num_warps >= traffic.num_lanes for traffic_smem mode.",
                self.config.num_lanes,
                new_core_id,
                lane_cap
            );
            let lanes = (0..self.config.num_lanes).map(LaneState::new).collect();
            self.core_states.push(CoreState {
                core_id: new_core_id,
                lanes,
                barrier: PatternBarrierState {
                    current_pattern_idx: 0,
                    phase: BarrierPhase::Running,
                    pattern_start_cycle: 0,
                },
                checkpoints: Vec::new(),
                completion_route: HashMap::new(),
                next_request_id: 1,
                done_logged: false,
            });
        }
    }

    fn route_completions(state: &mut CoreState, events: &[crate::muon::gmem::SmemCompletionEvent]) {
        for event in events {
            let lane_id = match state.completion_route.remove(&event.request_id) {
                Some(lane_id) => lane_id,
                None => continue,
            };
            if let Some(lane) = state.lanes.get_mut(lane_id) {
                lane.inflight = lane.inflight.saturating_sub(1);
                lane.completed_in_pattern = lane.completed_in_pattern.saturating_add(1);
                if lane.phase == LanePhase::Drain && lane.inflight == 0 {
                    lane.phase = LanePhase::BarrierWait;
                }
            }
        }
    }

    fn tick_lane_issue(
        state: &mut CoreState,
        lane_idx: usize,
        timing: &mut CoreTimingModel,
        now: Cycle,
        pattern_idx: usize,
        patterns: &PatternEngine,
        reqs_per_pattern: u32,
        max_inflight: usize,
        retry_backoff: Cycle,
    ) {
        let lane = &mut state.lanes[lane_idx];

        match lane.phase {
            LanePhase::WaitRetry => {
                if now >= lane.retry_at {
                    lane.phase = LanePhase::ActiveIssue;
                } else {
                    return;
                }
            }
            LanePhase::Drain => {
                if lane.inflight == 0 {
                    lane.phase = LanePhase::BarrierWait;
                }
                return;
            }
            LanePhase::BarrierWait | LanePhase::Finished => return,
            LanePhase::ActiveIssue => {}
        }

        if lane.issued_in_pattern >= reqs_per_pattern {
            lane.phase = if lane.inflight == 0 {
                LanePhase::BarrierWait
            } else {
                LanePhase::Drain
            };
            return;
        }
        if lane.inflight >= max_inflight {
            return;
        }
        if now < lane.retry_at {
            lane.phase = LanePhase::WaitRetry;
            return;
        }

        let pattern = patterns
            .pattern(pattern_idx)
            .expect("pattern index out of range in lane issue");
        let addr = patterns
            .lane_addr(pattern_idx, lane.next_t, lane.lane_id)
            .expect("missing lane address for pattern");
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);
        let request = SmemRequest {
            id: request_id,
            warp: lane.source_warp,
            addr,
            lane_addrs: Some(vec![addr]),
            bytes: pattern.req_bytes,
            active_lanes: 1,
            is_store: pattern.op.is_store(),
            bank: 0,
            subbank: 0,
        };

        match timing.issue_smem_request_frontend(now, request) {
            Ok(_) => {
                state.completion_route.insert(request_id, lane_idx);
                lane.last_request_id = Some(request_id);
                lane.inflight = lane.inflight.saturating_add(1);
                lane.issued_in_pattern = lane.issued_in_pattern.saturating_add(1);
                lane.next_t = lane.next_t.saturating_add(1);
                if lane.issued_in_pattern >= reqs_per_pattern {
                    lane.phase = LanePhase::Drain;
                }
            }
            Err(retry_at) => {
                lane.retry_at = retry_at.max(now.saturating_add(retry_backoff));
                lane.phase = LanePhase::WaitRetry;
            }
        }
    }

    fn tick_core_lockstep(
        state: &mut CoreState,
        timing: &mut CoreTimingModel,
        now: Cycle,
        patterns: &PatternEngine,
        reqs_per_pattern: u32,
        max_inflight: usize,
        retry_backoff: Cycle,
    ) -> Option<PatternCheckpoint> {
        if state.is_done() {
            return None;
        }
        if patterns.len() == 0 {
            state.barrier.phase = BarrierPhase::Done;
            return None;
        }
        if state.barrier.phase == BarrierPhase::Done {
            return None;
        }

        let mut checkpoint: Option<PatternCheckpoint> = None;
        let pattern_idx = state.barrier.current_pattern_idx;

        if state.barrier.phase == BarrierPhase::Running {
            for lane_idx in 0..state.lanes.len() {
                Self::tick_lane_issue(
                    state,
                    lane_idx,
                    timing,
                    now,
                    pattern_idx,
                    patterns,
                    reqs_per_pattern,
                    max_inflight,
                    retry_backoff,
                );
            }

            let all_waiting = state.lanes.iter().all(|lane| {
                matches!(
                    lane.phase,
                    LanePhase::Drain | LanePhase::BarrierWait | LanePhase::Finished
                )
            });
            if all_waiting {
                state.barrier.phase = BarrierPhase::WaitingDrain;
            }
        }

        if state.barrier.phase == BarrierPhase::WaitingDrain {
            let all_drained = state.lanes.iter().all(|lane| lane.inflight == 0);
            if all_drained {
                let pattern_name = patterns
                    .pattern_name(pattern_idx)
                    .unwrap_or("unknown")
                    .to_string();
                let duration = now.saturating_sub(state.barrier.pattern_start_cycle);
                let cp = PatternCheckpoint {
                    core_id: state.core_id,
                    pattern_idx,
                    pattern_name,
                    finished_cycle: now,
                    duration_cycles: duration,
                };
                state.checkpoints.push(cp.clone());
                checkpoint = Some(cp);
                state.barrier.phase = BarrierPhase::Advance;
            }
        }

        if state.barrier.phase == BarrierPhase::Advance {
            if state.barrier.current_pattern_idx + 1 < patterns.len() {
                state.barrier.current_pattern_idx =
                    state.barrier.current_pattern_idx.saturating_add(1);
                state.barrier.pattern_start_cycle = now;
                state.barrier.phase = BarrierPhase::Running;
                for lane in &mut state.lanes {
                    lane.reset_for_pattern(state.barrier.current_pattern_idx, now);
                }
            } else {
                for lane in &mut state.lanes {
                    lane.phase = LanePhase::Finished;
                }
                state.barrier.phase = BarrierPhase::Done;
            }
        }

        checkpoint
    }

    fn tick_core_independent(
        state: &mut CoreState,
        timing: &mut CoreTimingModel,
        now: Cycle,
        patterns: &PatternEngine,
        reqs_per_pattern: u32,
        max_inflight: usize,
        retry_backoff: Cycle,
    ) -> Option<PatternCheckpoint> {
        if patterns.is_empty() {
            state.barrier.phase = BarrierPhase::Done;
            return None;
        }

        for lane_idx in 0..state.lanes.len() {
            let lane_pattern_idx = state.lanes[lane_idx].current_pattern_idx;
            if lane_pattern_idx >= patterns.len() {
                state.lanes[lane_idx].phase = LanePhase::Finished;
                continue;
            }
            Self::tick_lane_issue(
                state,
                lane_idx,
                timing,
                now,
                lane_pattern_idx,
                patterns,
                reqs_per_pattern,
                max_inflight,
                retry_backoff,
            );

            if state.lanes[lane_idx].phase == LanePhase::Drain
                && state.lanes[lane_idx].inflight == 0
            {
                let next_pattern = state.lanes[lane_idx].current_pattern_idx.saturating_add(1);
                if next_pattern < patterns.len() {
                    state.lanes[lane_idx].reset_for_pattern(next_pattern, now);
                } else {
                    state.lanes[lane_idx].phase = LanePhase::Finished;
                }
            }
        }

        if state
            .lanes
            .iter()
            .all(|lane| lane.phase == LanePhase::Finished && lane.inflight == 0)
            && state.completion_route.is_empty()
        {
            state.barrier.phase = BarrierPhase::Done;
        }
        None
    }

    fn maybe_emit_results(&mut self) {
        if !self.done || self.results_emitted {
            return;
        }
        self.results_emitted = true;

        let checkpoints = self.collect_checkpoints();
        let metadata = json!({
            "mode": "traffic_smem",
            "lockstep_patterns": self.config.lockstep_patterns,
            "num_lanes": self.config.num_lanes,
            "reqs_per_pattern": self.config.reqs_per_pattern,
            "pattern_count": self.pattern_engine.len(),
        });

        if let Some(path) = self.config.logging.results_json.as_deref() {
            if let Err(err) = TrafficLogger::write_json(path, &checkpoints, metadata.clone()) {
                eprintln!("failed to write traffic JSON '{}': {}", path, err);
            }
        }
        if let Some(path) = self.config.logging.results_csv.as_deref() {
            if let Err(err) = TrafficLogger::write_csv(path, &checkpoints) {
                eprintln!("failed to write traffic CSV '{}': {}", path, err);
            }
        }
    }

    fn collect_checkpoints(&self) -> Vec<PatternCheckpoint> {
        let mut out: Vec<PatternCheckpoint> = self
            .core_states
            .iter()
            .flat_map(|core| core.checkpoints.iter().cloned())
            .collect();
        out.sort_by_key(|x| (x.core_id, x.pattern_idx, x.finished_cycle));
        out
    }
}
