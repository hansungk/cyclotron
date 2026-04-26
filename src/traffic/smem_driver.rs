use serde_json::json;

use crate::base::behavior::ModuleBehaviors;
use crate::muon::core::MuonCore;
use crate::timeflow::smem::SmemPatternRun;
use crate::timeq::Cycle;
use crate::traffic::config::TrafficConfig;
use crate::traffic::logging::{PatternCheckpoint, TrafficLogger};
use crate::traffic::patterns::PatternEngine;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct CoreState {
    core_id: usize,
    current_pattern_idx: usize,
    current_cycle: Cycle,
    checkpoints: Vec<PatternCheckpoint>,
    done: bool,
    done_logged: bool,
}

impl CoreState {
    fn new(core_id: usize) -> Self {
        Self {
            core_id,
            current_pattern_idx: 0,
            current_cycle: 0,
            checkpoints: Vec::new(),
            done: false,
            done_logged: false,
        }
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

        self.ensure_core_state(core_id);

        let num_lanes = self.config.num_lanes;
        let print_lines = self.config.logging.print_traffic_lines;
        let total_patterns = self.pattern_engine.len();

        let state = &mut self.core_states[core_id];
        if state.done {
            core.scheduler.tick_one();
            self.check_all_done();
            return;
        }

        let mut suite_order: Vec<String> = Vec::new();
        let mut grouped: BTreeMap<String, Vec<SmemPatternRun>> = BTreeMap::new();
        for pattern_idx in 0..total_patterns {
            let pattern = self
                .pattern_engine
                .pattern(pattern_idx)
                .expect("pattern index out of range");
            if !grouped.contains_key(&pattern.suite) {
                suite_order.push(pattern.suite.clone());
            }
            let active_lanes = pattern.active_lanes.max(1).min(num_lanes.max(1));
            let addrs: Vec<Vec<u64>> = (0..pattern.reqs_per_pattern)
                .map(|wave_t| {
                    (0..active_lanes)
                        .map(|lane| {
                            self.pattern_engine
                                .lane_addr(pattern_idx, wave_t, lane)
                                .expect("missing lane address")
                        })
                        .collect()
                })
                .collect();
            grouped
                .entry(pattern.suite.clone())
                .or_default()
                .push(SmemPatternRun {
                    stream_id: 0,
                    pattern_idx,
                    suite: pattern.suite.clone(),
                    pattern_name: self
                        .pattern_engine
                        .pattern_name(pattern_idx)
                        .unwrap_or("unknown")
                        .to_string(),
                    addrs,
                    is_read: !pattern.op.is_store(),
                    req_bytes: pattern.req_bytes.max(1),
                    active_lanes,
                    max_inflight_per_lane: pattern.max_inflight_per_lane.max(1),
                    issue_gap: pattern.issue_gap,
                    working_set_bytes: self.config.address.smem_size_bytes,
                });
        }

        let mut streams: Vec<Vec<SmemPatternRun>> = Vec::new();
        for (stream_id, suite) in suite_order.iter().enumerate() {
            if let Some(mut stream) = grouped.remove(suite) {
                for run in &mut stream {
                    run.stream_id = stream_id;
                }
                streams.push(stream);
            }
        }

        let model_present = core
            .with_timing_model(|timing, _scheduler, _now| timing.simulate_smem_patterns(streams));

        let results = match model_present {
            Some(results) => results,
            None => panic!("frontend_mode=traffic_smem requires sim.timing=true"),
        };

        for result in results {
            state.current_cycle = state.current_cycle.max(result.finished_cycle);
            state.checkpoints.push(PatternCheckpoint {
                core_id: state.core_id,
                pattern_idx: result.pattern_idx,
                pattern_name: result.pattern_name.clone(),
                finished_cycle: result.finished_cycle,
                duration_cycles: result.duration_cycles,
            });
            if print_lines {
                TrafficLogger::log_pattern_checkpoint_with_metadata(
                    "smem",
                    &result.suite,
                    core_id,
                    &result.pattern_name,
                    result.finished_cycle,
                    result.duration_cycles,
                    result.reqs_per_lane,
                    result.active_lanes,
                    result.issue_gap,
                    result.max_outstanding,
                    result.working_set_bytes,
                );
            }
        }

        state.current_pattern_idx = total_patterns;
        state.done = true;
        if !state.done_logged {
            state.done_logged = true;
            if print_lines {
                TrafficLogger::log_core_done("smem", core_id, state.current_cycle);
            }
        }

        core.scheduler.tick_one();
        self.check_all_done();
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn ensure_core_state(&mut self, core_id: usize) {
        while self.core_states.len() <= core_id {
            let id = self.core_states.len();
            self.core_states.push(CoreState::new(id));
        }
    }

    fn check_all_done(&mut self) {
        self.done = self.core_states.iter().all(|s| s.done);
        self.maybe_emit_results();
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
