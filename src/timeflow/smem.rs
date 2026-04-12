use std::collections::VecDeque;
use std::ops::AddAssign;

use crate::timeflow::{graph::FlowGraph, types::CoreFlowPayload};
use crate::timeq::{Cycle, Ticket};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemType {
    TwoPort,
    TwoReadOneWrite,
}

impl Default for MemType {
    fn default() -> Self {
        MemType::TwoPort
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmemSerialization {
    CoreSerialized,
    FullySerialized,
    NotSerialized,
}

impl Default for SmemSerialization {
    fn default() -> Self {
        SmemSerialization::CoreSerialized
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmemStats {
    pub issued: u64,
    pub read_issued: u64,
    pub write_issued: u64,
    pub completed: u64,
    pub read_completed: u64,
    pub write_completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
    pub bytes_issued: u64,
    pub bytes_completed: u64,
    pub inflight: u64,
    pub max_inflight: u64,
    pub max_completion_queue: u64,
    pub last_completion_cycle: Option<Cycle>,
    pub sample_cycles: u64,
    pub bank_busy_samples: Vec<u64>,
    pub bank_read_busy_samples: Vec<u64>,
    pub bank_write_busy_samples: Vec<u64>,
    pub bank_attempts: Vec<u64>,
    pub bank_conflicts: Vec<u64>,
}

impl AddAssign<&SmemStats> for SmemStats {
    fn add_assign(&mut self, other: &SmemStats) {
        self.issued = self.issued.saturating_add(other.issued);
        self.read_issued = self.read_issued.saturating_add(other.read_issued);
        self.write_issued = self.write_issued.saturating_add(other.write_issued);
        self.completed = self.completed.saturating_add(other.completed);
        self.read_completed = self.read_completed.saturating_add(other.read_completed);
        self.write_completed = self.write_completed.saturating_add(other.write_completed);
        self.queue_full_rejects = self
            .queue_full_rejects
            .saturating_add(other.queue_full_rejects);
        self.busy_rejects = self.busy_rejects.saturating_add(other.busy_rejects);
        self.bytes_issued = self.bytes_issued.saturating_add(other.bytes_issued);
        self.bytes_completed = self.bytes_completed.saturating_add(other.bytes_completed);
        self.inflight = self.inflight.saturating_add(other.inflight);
        self.max_inflight = self.max_inflight.max(other.max_inflight);
        self.max_completion_queue = self.max_completion_queue.max(other.max_completion_queue);
        self.last_completion_cycle = match (self.last_completion_cycle, other.last_completion_cycle)
        {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, Some(b)) => Some(b),
            (a, None) => a,
        };
        self.sample_cycles = self.sample_cycles.saturating_add(other.sample_cycles);
        merge_vec_sums(&mut self.bank_busy_samples, &other.bank_busy_samples);
        merge_vec_sums(
            &mut self.bank_read_busy_samples,
            &other.bank_read_busy_samples,
        );
        merge_vec_sums(
            &mut self.bank_write_busy_samples,
            &other.bank_write_busy_samples,
        );
        merge_vec_sums(&mut self.bank_attempts, &other.bank_attempts);
        merge_vec_sums(&mut self.bank_conflicts, &other.bank_conflicts);
    }
}

fn merge_vec_sums(dst: &mut Vec<u64>, src: &[u64]) {
    if dst.len() < src.len() {
        dst.resize(src.len(), 0);
    }
    for (idx, value) in src.iter().enumerate() {
        dst[idx] = dst[idx].saturating_add(*value);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SmemUtilSample {
    pub lane_busy: usize,
    pub lane_total: usize,
    pub bank_busy: usize,
    pub bank_total: usize,
}

#[derive(Debug, Clone)]
pub struct SmemRequest {
    pub id: u64,
    pub warp: usize,
    pub addr: u64,
    pub lane_addrs: Option<Vec<u64>>,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_store: bool,
    pub bank: usize,
    pub subbank: usize,
}

impl SmemRequest {
    pub fn new(warp: usize, bytes: u32, active_lanes: u32, is_store: bool, bank: usize) -> Self {
        Self {
            id: 0,
            warp,
            addr: 0,
            lane_addrs: None,
            bytes,
            active_lanes,
            is_store,
            bank,
            subbank: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SmemCompletion {
    pub request: SmemRequest,
    pub ticket_ready_at: Cycle,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone)]
pub struct SmemIssue {
    pub request_id: u64,
    pub ticket: Ticket,
}

pub type SmemReject = crate::timeflow::types::RejectWith<SmemRequest>;
pub use crate::timeflow::types::RejectReason as SmemRejectReason;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SmemFlowConfig {
    pub smem_size: u64,
    pub num_banks: usize,
    pub num_subbanks: usize,
    pub word_bytes: u32,
    pub prealign_buf_depth: usize,
    pub mem_type: MemType,
    pub stride_by_word: bool,
    pub serialization: SmemSerialization,
    pub num_lanes: usize,
    pub base_overhead: u64,
    pub read_extra: u64,
    pub max_outstanding: usize,
}

impl SmemFlowConfig {
    pub fn bank_size_bytes(&self) -> u64 {
        self.smem_size / self.num_banks.max(1) as u64
    }

    pub fn num_ports(&self) -> usize {
        if self.stride_by_word {
            self.num_subbanks
        } else {
            self.num_banks.max(1)
        }
    }

    pub fn read_ports_per_subbank(&self) -> usize {
        match self.mem_type {
            MemType::TwoPort => 1,
            MemType::TwoReadOneWrite => 2,
        }
    }

    pub fn write_ports_per_subbank(&self) -> usize {
        1
    }
}

impl Default for SmemFlowConfig {
    fn default() -> Self {
        Self {
            smem_size: 128 << 10,
            num_banks: 4,
            num_subbanks: 16,
            word_bytes: 4,
            prealign_buf_depth: 2,
            mem_type: MemType::default(),
            stride_by_word: true,
            serialization: SmemSerialization::default(),
            num_lanes: 16,
            base_overhead: 5,
            read_extra: 1,
            max_outstanding: 16,
        }
    }
}


fn chisel_rr_winner(valid_bits: u32, mask: u32, n_inputs: usize) -> (usize, u32) {
    let all_bits = (1u64 << n_inputs) as u32 - 1;
    let valid = valid_bits & all_bits;
    let filter_val =
        (((valid & !mask & all_bits) as u64) << n_inputs) as u64 | valid as u64;
    // rightOR
    let mut ro = filter_val;
    let mut shift = 1u64;
    while shift < (2 * n_inputs) as u64 {
        ro |= ro >> shift;
        shift <<= 1;
    }
    let double_mask = (1u64 << (2 * n_inputs)) - 1;
    ro &= double_mask;
    let unready = (ro >> 1) | ((mask as u64) << n_inputs);
    let upper = ((unready >> n_inputs) & (all_bits as u64)) as u32;
    let lower = (unready & (all_bits as u64)) as u32;
    let readys = !(upper & lower) & all_bits;
    let grant = readys & valid;
    if grant == 0 {
        return (0, mask);
    }
    let winner = grant.trailing_zeros() as usize;
    // leftOR of grant to produce new mask
    let mut lm = grant as u64;
    let mut s = 1u64;
    while s < n_inputs as u64 {
        lm |= lm << s;
        s <<= 1;
    }
    (winner, (lm as u32) & all_bits)
}

pub(crate) struct SmemSubgraph {
    config: SmemFlowConfig,
    pub(crate) completions: VecDeque<SmemCompletion>,
    total_inflight: u64,
    next_id: u64,
    pub(crate) stats: SmemStats,
    pending: VecDeque<(Cycle, SmemCompletion)>,
    issue_rr_masks: Vec<u32>,
}

impl SmemSubgraph {
    pub fn attach(_graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
        let num_banks = config.num_banks.max(1);
        let n_ports = if config.stride_by_word {
            config.num_subbanks
        } else {
            num_banks
        };
        let mut stats = SmemStats::default();
        stats.bank_busy_samples = vec![0; num_banks];
        stats.bank_read_busy_samples = vec![0; num_banks];
        stats.bank_write_busy_samples = vec![0; num_banks];
        stats.bank_attempts = vec![0; num_banks];
        stats.bank_conflicts = vec![0; num_banks];

        let n_inputs = config.num_lanes + 1;
        let initial_mask = (1u32 << n_inputs) - 1;

        Self {
            config: config.clone(),
            completions: VecDeque::new(),
            total_inflight: 0,
            next_id: 0,
            stats,
            pending: VecDeque::new(),
            issue_rr_masks: vec![initial_mask; n_ports],
        }
    }

    pub fn sample_and_accumulate(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>) {
        self.stats.sample_cycles = self.stats.sample_cycles.saturating_add(1);
    }

    pub fn sample_utilization(&self, _graph: &mut FlowGraph<CoreFlowPayload>) -> SmemUtilSample {
        SmemUtilSample {
            lane_busy: 0,
            lane_total: self.config.num_lanes,
            bank_busy: 0,
            bank_total: self.config.num_banks.max(1),
        }
    }

    pub fn simulate_pattern(&self, addrs: &[Vec<u64>], is_read: bool) -> u64 {
        let cfg = &self.config;
        let n_lanes = cfg.num_lanes;
        let n_waves = addrs.len();
        if n_waves == 0 || n_lanes == 0 {
            return 0;
        }

        let n_ports = if cfg.stride_by_word {
            cfg.num_subbanks
        } else {
            cfg.num_banks.max(1)
        };
        let n_inputs = n_lanes + 1;
        let initial_mask = (1u32 << n_inputs) - 1;
        let mut rr_masks = vec![initial_mask; n_ports];

        let ports: Vec<Vec<usize>> = addrs
            .iter()
            .map(|wave_addrs| {
                wave_addrs
                    .iter()
                    .map(|&a| {
                        if cfg.stride_by_word {
                            ((a / cfg.word_bytes as u64) % cfg.num_subbanks as u64) as usize
                        } else {
                            ((a / cfg.bank_size_bytes()) % cfg.num_banks as u64) as usize
                        }
                    })
                    .collect()
            })
            .collect();

        let ports_this_cycle = if is_read {
            cfg.read_ports_per_subbank()
        } else {
            cfg.write_ports_per_subbank()
        };

        let mut req_idx = vec![0usize; n_lanes];
        let mut cycle: u64 = 0;
        let mut max_accept_cycle: u64 = 0;
        let max_cycles = n_waves as u64 * 20;

        loop {
            // Collect active lanes
            let active: Vec<usize> = (0..n_lanes)
                .filter(|&ln| req_idx[ln] < n_waves)
                .collect();
            if active.is_empty() {
                break;
            }

            // Coalescing check: all active lanes same address?
            let leader_addr = addrs[req_idx[active[0]]][active[0]];
            let coalescible = active
                .iter()
                .all(|&ln| addrs[req_idx[ln]][ln] == leader_addr);

            if coalescible {
                // All same address → coalesce, advance all lanes
                for &ln in &active {
                    req_idx[ln] += 1;
                }
                max_accept_cycle = max_accept_cycle.max(cycle + 1);
            } else {
                // Per-port arbitration
                let mut port_candidates: Vec<Vec<usize>> = vec![Vec::new(); n_ports];
                for &ln in &active {
                    let pid = ports[req_idx[ln]][ln];
                    port_candidates[pid].push(ln);
                }

                for pid in 0..n_ports {
                    let candidates = &port_candidates[pid];
                    if candidates.is_empty() {
                        continue;
                    }
                    let mut valid_bits: u32 = 0;
                    for &ln in candidates {
                        valid_bits |= 1 << ln;
                    }
                    // Allow up to ports_this_cycle winners per subbank
                    for _ in 0..ports_this_cycle {
                        if valid_bits == 0 {
                            break;
                        }
                        let (winner, new_mask) =
                            chisel_rr_winner(valid_bits, rr_masks[pid], n_inputs);
                        rr_masks[pid] = new_mask;
                        req_idx[winner] += 1;
                        max_accept_cycle = max_accept_cycle.max(cycle + 1);
                        valid_bits &= !(1 << winner);
                    }
                }
            }

            cycle += 1;
            if cycle > max_cycles {
                break;
            }
        }

        let mut total = max_accept_cycle + cfg.base_overhead - 1;
        if is_read {
            total += cfg.read_extra;
        }
        total
    }

    pub fn issue(
        &mut self,
        _graph: &mut FlowGraph<CoreFlowPayload>,
        now: Cycle,
        mut request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        // Assign request ID
        let assigned_id = if request.id == 0 {
            let id = self.next_id;
            self.next_id += 1;
            id
        } else {
            self.next_id = self.next_id.max(request.id + 1);
            request.id
        };
        request.id = assigned_id;

        // Check total outstanding limit
        if self.total_inflight >= self.config.max_outstanding as u64 {
            self.stats.queue_full_rejects += 1;
            return Err(SmemReject {
                payload: request,
                retry_at: now.saturating_add(1),
                reason: SmemRejectReason::QueueFull,
            });
        }

        // Compute wave cost via single-wave arbitration simulation
        let wave_cost = self.simulate_single_wave(&request);

        let is_store = request.is_store;
        let bytes = request.bytes;

        let mut ready_at = now + wave_cost + self.config.base_overhead;
        if !is_store {
            ready_at += self.config.read_extra;
        }

        // Update stats
        self.total_inflight += 1;
        self.stats.issued += 1;
        if is_store {
            self.stats.write_issued += 1;
        } else {
            self.stats.read_issued += 1;
        }
        self.stats.bytes_issued += bytes as u64;
        self.stats.inflight = self.total_inflight;
        self.stats.max_inflight = self.stats.max_inflight.max(self.total_inflight);

        let ticket = Ticket::new(now, ready_at, bytes);

        // Schedule completion
        self.pending.push_back((
            ready_at,
            SmemCompletion {
                request,
                ticket_ready_at: ready_at,
                completed_at: ready_at,
            },
        ));

        Ok(SmemIssue {
            request_id: assigned_id,
            ticket,
        })
    }

    // Tick the model and collect completions.
    pub fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        // Drain pending completions that are ready
        while let Some(&(ready_at, _)) = self.pending.front() {
            if now >= ready_at {
                let (_, mut completion) = self.pending.pop_front().unwrap();
                completion.completed_at = now;

                let is_store = completion.request.is_store;
                let bytes = completion.request.bytes;
                self.stats.completed += 1;
                if is_store {
                    self.stats.write_completed += 1;
                } else {
                    self.stats.read_completed += 1;
                }
                self.stats.bytes_completed += bytes as u64;
                self.total_inflight = self.total_inflight.saturating_sub(1);
                self.stats.inflight = self.total_inflight;
                self.stats.last_completion_cycle = Some(now);
                self.completions.push_back(completion);
            } else {
                break;
            }
        }
        self.stats.max_completion_queue = self
            .stats
            .max_completion_queue
            .max(self.completions.len() as u64);
    }

    fn simulate_single_wave(&mut self, request: &SmemRequest) -> u64 {
        let cfg = &self.config;
        let n_lanes = cfg.num_lanes;
        let n_ports = if cfg.stride_by_word {
            cfg.num_subbanks
        } else {
            cfg.num_banks.max(1)
        };
        let n_inputs = n_lanes + 1;

        let lane_addrs = match &request.lane_addrs {
            Some(addrs) => addrs.clone(),
            None => vec![request.addr; n_lanes],
        };

        // Compute port for each lane
        let lane_ports: Vec<usize> = lane_addrs
            .iter()
            .map(|&a| {
                if cfg.stride_by_word {
                    ((a / cfg.word_bytes as u64) % cfg.num_subbanks as u64) as usize
                } else {
                    ((a / cfg.bank_size_bytes()) % cfg.num_banks as u64) as usize
                }
            })
            .collect();

        // Check coalescing
        let all_same = lane_addrs.iter().all(|&a| a == lane_addrs[0]);
        if all_same {
            return 1;
        }

        // Determine how many winners per subbank per cycle
        let ports_this_cycle = if request.is_store {
            cfg.write_ports_per_subbank()
        } else {
            cfg.read_ports_per_subbank()
        };

        // Simulate cycle-by-cycle arbitration
        let mut served = vec![false; n_lanes];
        let mut cycles: u64 = 0;

        loop {
            let remaining: Vec<usize> = (0..n_lanes)
                .filter(|&ln| !served[ln])
                .collect();
            if remaining.is_empty() {
                break;
            }

            // Per-port arbitration
            let mut port_candidates: Vec<Vec<usize>> = vec![Vec::new(); n_ports];
            for &ln in &remaining {
                port_candidates[lane_ports[ln]].push(ln);
            }

            for pid in 0..n_ports {
                let candidates = &port_candidates[pid];
                if candidates.is_empty() {
                    continue;
                }
                let mut valid_bits: u32 = 0;
                for &ln in candidates {
                    valid_bits |= 1 << ln;
                }
                // Allow up to ports_this_cycle winners per subbank
                for _ in 0..ports_this_cycle {
                    if valid_bits == 0 {
                        break;
                    }
                    let (winner, new_mask) =
                        chisel_rr_winner(valid_bits, self.issue_rr_masks[pid], n_inputs);
                    self.issue_rr_masks[pid] = new_mask;
                    served[winner] = true;
                    valid_bits &= !(1 << winner);
                }
            }

            cycles += 1;
            if cycles > n_lanes as u64 * 4 {
                break; // safety limit
            }
        }

        cycles
    }
}

pub fn extract_smem_request(
    request: crate::timeq::ServiceRequest<CoreFlowPayload>,
) -> SmemRequest {
    match request.payload {
        CoreFlowPayload::Smem(req) => req,
        _ => panic!("expected smem request"),
    }
}
