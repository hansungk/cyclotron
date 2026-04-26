use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::AddAssign;

use crate::timeflow::{graph::FlowGraph, types::CoreFlowPayload};
use crate::timeq::{
    AcceptStatus, Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemType {
    TwoPort,
    TwoReadOneWrite,
}

impl Default for MemType {
    fn default() -> Self {
        Self::TwoPort
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
        Self::CoreSerialized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmemAddressMap {
    // Bank selection follows the physical contiguous-bank layout in RTL:
    //   bank = (addr / bank_size_bytes) % num_banks
    //   subbank = (addr / word_bytes) % num_subbanks
    // This is the default because it matches Radiance shared-memory manager decoding.
    RtlContiguousBanks,
    // Legacy interleaved map used by older Cyclotron paths.
    SubbankThenBank,
    // Legacy interleaved map used by some split/conflict helper code.
    BankThenSubbank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmemClientKind {
    Muon,
    Gemmini,
    External,
}

impl Default for SmemClientKind {
    fn default() -> Self {
        Self::Muon
    }
}

impl Default for SmemAddressMap {
    fn default() -> Self {
        Self::RtlContiguousBanks
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
    pub lane_ids: Option<Vec<usize>>,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_store: bool,
    pub client: SmemClientKind,
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
            lane_ids: None,
            bytes,
            active_lanes,
            is_store,
            client: SmemClientKind::Muon,
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

#[derive(Debug, Clone)]
pub struct SmemPatternRun {
    pub stream_id: usize,
    pub pattern_idx: usize,
    pub suite: String,
    pub pattern_name: String,
    pub addrs: Vec<Vec<u64>>,
    pub is_read: bool,
    pub req_bytes: u32,
    pub active_lanes: usize,
    pub max_inflight_per_lane: usize,
    pub issue_gap: u64,
    pub working_set_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SmemPatternResult {
    pub stream_id: usize,
    pub pattern_idx: usize,
    pub suite: String,
    pub pattern_name: String,
    pub finished_cycle: Cycle,
    pub duration_cycles: Cycle,
    pub reqs_per_lane: u32,
    pub active_lanes: usize,
    pub issue_gap: u64,
    pub max_outstanding: usize,
    pub working_set_bytes: u64,
}

#[derive(Debug)]
struct ActivePatternState {
    run: SmemPatternRun,
    start_cycle: Cycle,
    req_idx: Vec<usize>,
    inflight: Vec<usize>,
    next_issue_at: Vec<Cycle>,
    completed_lane_reqs: usize,
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
    pub address_map: SmemAddressMap,
    pub num_lanes: usize,
    pub base_overhead: u64,
    pub read_extra: u64,
    pub max_outstanding: usize,
    pub prealign_latency: u64,
    pub prealign_bytes_per_cycle: u32,
    pub crossbar_latency: u64,
    pub crossbar_bytes_per_cycle: u32,
    pub port_latency_read: u64,
    pub port_latency_write: u64,
    pub bank_read_bytes_per_cycle: u32,
    pub bank_write_bytes_per_cycle: u32,
    pub tail_bytes_per_cycle: u32,
    pub port_queue_depth: usize,
    pub completion_queue_depth: usize,
}

impl SmemFlowConfig {
    pub fn bank_size_bytes(&self) -> u64 {
        let banks = self.num_banks.max(1) as u64;
        self.smem_size / banks
    }

    pub fn active_subbanks(&self) -> usize {
        if self.stride_by_word {
            self.num_subbanks.max(1)
        } else {
            1
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

    pub fn crossbar_inputs(&self) -> usize {
        match self.serialization {
            SmemSerialization::FullySerialized => 1,
            SmemSerialization::CoreSerialized | SmemSerialization::NotSerialized => {
                self.num_lanes.max(1)
            }
        }
    }

    pub fn crossbar_outputs(&self) -> usize {
        // Radiance's Muon path first routes through `alignment_xbar` into one
        // output per SMEM word/subbank. The bank-specific uniform xbars sit
        // after that split. Modeling the first xbar as (bank, subbank) outputs
        // admits parallelism that the RTL does not have.
        self.active_subbanks().max(1)
    }

    pub fn decode_addr(&self, addr: u64) -> (usize, usize) {
        let num_banks = self.num_banks.max(1);
        let num_subbanks = self.active_subbanks().max(1);
        let word_bytes = self.word_bytes.max(1) as u64;

        if !self.stride_by_word {
            let bank = ((addr / self.bank_size_bytes().max(1)) % num_banks as u64) as usize;
            return (bank, 0);
        }

        let word = addr / word_bytes;
        match self.address_map {
            SmemAddressMap::RtlContiguousBanks => {
                let bank = ((addr / self.bank_size_bytes().max(1)) % num_banks as u64) as usize;
                let subbank = (word % num_subbanks as u64) as usize;
                (bank, subbank)
            }
            SmemAddressMap::SubbankThenBank => {
                let subbank = (word % num_subbanks as u64) as usize;
                let bank = ((word / num_subbanks as u64) % num_banks as u64) as usize;
                (bank, subbank)
            }
            SmemAddressMap::BankThenSubbank => {
                let bank = (word % num_banks as u64) as usize;
                let subbank = ((word / num_banks as u64) % num_subbanks as u64) as usize;
                (bank, subbank)
            }
        }
    }

    pub fn xbar_output_for_target(&self, bank: usize, subbank: usize) -> usize {
        let subbanks = self.active_subbanks().max(1);
        let _ = bank;
        let sb = subbank.min(subbanks.saturating_sub(1));
        sb
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
            address_map: SmemAddressMap::default(),
            num_lanes: 16,
            base_overhead: 0,
            read_extra: 0,
            // RTL stimulus limits are per TileLink generator/source set. Keep
            // the structural SMEM fabric cap high enough that per-lane issue
            // limits, not an artificial global queue, control replay pressure.
            max_outstanding: 512,
            prealign_latency: 0,
            prealign_bytes_per_cycle: 1024,
            crossbar_latency: 0,
            crossbar_bytes_per_cycle: 1024,
            port_latency_read: 0,
            port_latency_write: 0,
            bank_read_bytes_per_cycle: 1024,
            bank_write_bytes_per_cycle: 1024,
            tail_bytes_per_cycle: 1024,
            port_queue_depth: 64,
            completion_queue_depth: 64,
        }
    }
}

pub fn decode_smem_bank_subbank(config: &SmemFlowConfig, addr: u64) -> (usize, usize) {
    config.decode_addr(addr)
}

#[derive(Debug, Clone)]
struct LaneOp {
    request_id: u64,
    request_ids: Vec<u64>,
    lane: usize,
    addr: u64,
    bytes: u32,
    coalesced_ops: usize,
    bank: usize,
    subbank: usize,
    is_store: bool,
    client: SmemClientKind,
}

impl LaneOp {
    fn request_completion_counts(&self) -> HashMap<u64, usize> {
        let mut counts = HashMap::new();
        if self.request_ids.is_empty() {
            counts.insert(self.request_id, self.coalesced_ops.max(1));
            return counts;
        }
        for request_id in &self.request_ids {
            *counts.entry(*request_id).or_default() += 1;
        }
        counts
    }
}

#[derive(Debug, Clone)]
struct LaneIssueCandidate {
    lane: usize,
    addr: u64,
}

struct CoalescedReadyGroup {
    op: LaneOp,
    sources: Vec<usize>,
}

#[derive(Debug, Clone)]
struct InflightRequestState {
    request: SmemRequest,
    pending_ops: VecDeque<LaneOp>,
    ops_total: usize,
    ops_completed: usize,
    max_bank_ready_at: Cycle,
    tail_pending: bool,
    tail_enqueued: bool,
}

pub(crate) struct SmemSubgraph {
    config: SmemFlowConfig,
    pub(crate) completions: VecDeque<SmemCompletion>,
    total_inflight: u64,
    next_id: u64,
    pub(crate) stats: SmemStats,
    prealign: Vec<TimedServer<LaneOp>>,
    full_serial_gate: Option<TimedServer<LaneOp>>,
    read_xbar_input_queues: Vec<VecDeque<LaneOp>>,
    write_xbar_input_queues: Vec<VecDeque<LaneOp>>,
    read_xbar_output_servers: Vec<TimedServer<LaneOp>>,
    write_xbar_output_servers: Vec<TimedServer<LaneOp>>,
    read_xbar_rr_masks: Vec<u32>,
    write_xbar_rr_masks: Vec<u32>,
    bank_read_ports: Vec<Vec<Vec<TimedServer<LaneOp>>>>,
    bank_write_ports: Vec<Vec<Vec<TimedServer<LaneOp>>>>,
    gemmini_bank_read_ports: Vec<Vec<Vec<TimedServer<LaneOp>>>>,
    gemmini_bank_write_ports: Vec<Vec<Vec<TimedServer<LaneOp>>>>,
    bank_read_rr_next: Vec<Vec<usize>>,
    bank_write_rr_next: Vec<Vec<usize>>,
    gemmini_bank_read_rr_next: Vec<Vec<usize>>,
    gemmini_bank_write_rr_next: Vec<Vec<usize>>,
    read_tail: Vec<TimedServer<SmemCompletion>>,
    write_tail: Vec<TimedServer<SmemCompletion>>,
    inflight_requests: BTreeMap<u64, InflightRequestState>,
    bank_read_inflight: Vec<u64>,
    bank_write_inflight: Vec<u64>,
    gemmini_bank_read_inflight: Vec<u64>,
    gemmini_bank_write_inflight: Vec<u64>,
    last_lane_busy: usize,
    last_bank_busy: usize,
}

impl SmemSubgraph {
    pub fn attach(_graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
        let num_banks = config.num_banks.max(1);
        let active_subbanks = config.active_subbanks().max(1);

        let mut stats = SmemStats::default();
        stats.bank_busy_samples = vec![0; num_banks];
        stats.bank_read_busy_samples = vec![0; num_banks];
        stats.bank_write_busy_samples = vec![0; num_banks];
        stats.bank_attempts = vec![0; num_banks];
        stats.bank_conflicts = vec![0; num_banks];

        let prealign = (0..config.crossbar_inputs())
            .map(|_| {
                TimedServer::new(ServerConfig {
                    base_latency: config.prealign_latency,
                    bytes_per_cycle: config.prealign_bytes_per_cycle.max(1),
                    queue_capacity: config.prealign_buf_depth.max(1),
                    completions_per_cycle: u32::MAX,
                    ..ServerConfig::default()
                })
            })
            .collect();

        let full_serial_gate = if matches!(config.serialization, SmemSerialization::FullySerialized)
        {
            Some(TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 1,
                queue_capacity: config.port_queue_depth.max(1),
                completions_per_cycle: u32::MAX,
                ..ServerConfig::default()
            }))
        } else {
            None
        };

        let xbar_inputs = config.crossbar_inputs();
        let xbar_outputs = config.crossbar_outputs().max(1);
        let read_xbar_input_queues = (0..xbar_inputs)
            .map(|_| VecDeque::with_capacity(config.port_queue_depth.max(1)))
            .collect();
        let write_xbar_input_queues = (0..xbar_inputs)
            .map(|_| VecDeque::with_capacity(config.port_queue_depth.max(1)))
            .collect();
        let read_xbar_output_servers = (0..xbar_outputs)
            .map(|_| {
                TimedServer::new(ServerConfig {
                    base_latency: config.crossbar_latency,
                    bytes_per_cycle: config.crossbar_bytes_per_cycle.max(1),
                    queue_capacity: config.port_queue_depth.max(1),
                    completions_per_cycle: u32::MAX,
                    ..ServerConfig::default()
                })
            })
            .collect();
        let write_xbar_output_servers = (0..xbar_outputs)
            .map(|_| {
                TimedServer::new(ServerConfig {
                    base_latency: config.crossbar_latency,
                    bytes_per_cycle: config.crossbar_bytes_per_cycle.max(1),
                    queue_capacity: config.port_queue_depth.max(1),
                    completions_per_cycle: u32::MAX,
                    ..ServerConfig::default()
                })
            })
            .collect();
        let read_xbar_rr_masks = vec![xbar_rr_initial_mask(xbar_inputs); xbar_outputs];
        let write_xbar_rr_masks = vec![xbar_rr_initial_mask(xbar_inputs); xbar_outputs];

        let read_ports_per_subbank = config.read_ports_per_subbank();
        let write_ports_per_subbank = config.write_ports_per_subbank();
        let read_server_config = ServerConfig {
            base_latency: config.port_latency_read,
            bytes_per_cycle: config.bank_read_bytes_per_cycle.max(1),
            queue_capacity: config.port_queue_depth.max(1),
            completions_per_cycle: u32::MAX,
            ..ServerConfig::default()
        };
        let write_server_config = ServerConfig {
            base_latency: config.port_latency_write,
            bytes_per_cycle: config.bank_write_bytes_per_cycle.max(1),
            queue_capacity: config.port_queue_depth.max(1),
            completions_per_cycle: u32::MAX,
            ..ServerConfig::default()
        };
        let make_read_ports = || {
            (0..num_banks)
                .map(|_| {
                    (0..active_subbanks)
                        .map(|_| {
                            (0..read_ports_per_subbank)
                                .map(|_| TimedServer::new(read_server_config))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let make_write_ports = || {
            (0..num_banks)
                .map(|_| {
                    (0..active_subbanks)
                        .map(|_| {
                            (0..write_ports_per_subbank)
                                .map(|_| TimedServer::new(write_server_config))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let bank_read_ports = make_read_ports();
        let bank_write_ports = make_write_ports();
        let gemmini_bank_read_ports = make_read_ports();
        let gemmini_bank_write_ports = make_write_ports();
        let bank_read_rr_next = vec![vec![0; active_subbanks]; num_banks];
        let bank_write_rr_next = vec![vec![0; active_subbanks]; num_banks];
        let gemmini_bank_read_rr_next = vec![vec![0; active_subbanks]; num_banks];
        let gemmini_bank_write_rr_next = vec![vec![0; active_subbanks]; num_banks];

        let read_tail_config = ServerConfig {
            base_latency: config.base_overhead.saturating_add(config.read_extra),
            bytes_per_cycle: config.tail_bytes_per_cycle.max(1),
            queue_capacity: config.completion_queue_depth.max(1),
            completions_per_cycle: u32::MAX,
            ..ServerConfig::default()
        };

        let write_tail_config = ServerConfig {
            base_latency: config.base_overhead,
            // Writes already passed the RTL ack queue by this point; do not add
            // an extra byte-service cycle on top of the bank/write-ack path.
            bytes_per_cycle: u32::MAX,
            queue_capacity: config.completion_queue_depth.max(1),
            completions_per_cycle: u32::MAX,
            ..ServerConfig::default()
        };
        let tail_count = config.crossbar_inputs().max(1);
        let read_tail = (0..tail_count)
            .map(|_| TimedServer::new(read_tail_config))
            .collect();
        let write_tail = (0..tail_count)
            .map(|_| TimedServer::new(write_tail_config))
            .collect();

        Self {
            config: config.clone(),
            completions: VecDeque::new(),
            total_inflight: 0,
            next_id: 0,
            stats,
            prealign,
            full_serial_gate,
            read_xbar_input_queues,
            write_xbar_input_queues,
            read_xbar_output_servers,
            write_xbar_output_servers,
            read_xbar_rr_masks,
            write_xbar_rr_masks,
            bank_read_ports,
            bank_write_ports,
            gemmini_bank_read_ports,
            gemmini_bank_write_ports,
            bank_read_rr_next,
            bank_write_rr_next,
            gemmini_bank_read_rr_next,
            gemmini_bank_write_rr_next,
            read_tail,
            write_tail,
            inflight_requests: BTreeMap::new(),
            bank_read_inflight: vec![0; num_banks],
            bank_write_inflight: vec![0; num_banks],
            gemmini_bank_read_inflight: vec![0; num_banks],
            gemmini_bank_write_inflight: vec![0; num_banks],
            last_lane_busy: 0,
            last_bank_busy: 0,
        }
    }

    pub fn clear_stats(&mut self) {
        let num_banks = self.config.num_banks.max(1);
        self.stats = SmemStats::default();
        self.stats.bank_busy_samples = vec![0; num_banks];
        self.stats.bank_read_busy_samples = vec![0; num_banks];
        self.stats.bank_write_busy_samples = vec![0; num_banks];
        self.stats.bank_attempts = vec![0; num_banks];
        self.stats.bank_conflicts = vec![0; num_banks];
    }

    pub fn sample_and_accumulate(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>) {
        let (lane_busy, bank_busy, read_busy_banks, write_busy_banks) = self.busy_snapshot();
        self.last_lane_busy = lane_busy;
        self.last_bank_busy = bank_busy;
        self.stats.sample_cycles = self.stats.sample_cycles.saturating_add(1);

        for bank in 0..self.config.num_banks.max(1) {
            if read_busy_banks[bank] {
                self.stats.bank_read_busy_samples[bank] =
                    self.stats.bank_read_busy_samples[bank].saturating_add(1);
            }
            if write_busy_banks[bank] {
                self.stats.bank_write_busy_samples[bank] =
                    self.stats.bank_write_busy_samples[bank].saturating_add(1);
            }
            if read_busy_banks[bank] || write_busy_banks[bank] {
                self.stats.bank_busy_samples[bank] =
                    self.stats.bank_busy_samples[bank].saturating_add(1);
            }
        }
    }

    pub fn sample_utilization(&self, _graph: &mut FlowGraph<CoreFlowPayload>) -> SmemUtilSample {
        SmemUtilSample {
            lane_busy: self.last_lane_busy,
            lane_total: self.config.num_lanes.max(1),
            bank_busy: self.last_bank_busy,
            bank_total: self.config.num_banks.max(1),
        }
    }

    pub fn simulate_pattern_timed(
        &self,
        addrs: &[Vec<u64>],
        is_read: bool,
        req_bytes: u32,
        max_inflight_per_lane: usize,
        issue_gap: u64,
    ) -> u64 {
        if addrs.is_empty() || addrs.first().map(|x| x.is_empty()).unwrap_or(true) {
            return 0;
        }
        let run = SmemPatternRun {
            stream_id: 0,
            pattern_idx: 0,
            suite: "traffic_frontend".to_string(),
            pattern_name: "pattern".to_string(),
            addrs: addrs.to_vec(),
            is_read,
            req_bytes,
            active_lanes: addrs.first().map(|x| x.len()).unwrap_or(0),
            max_inflight_per_lane,
            issue_gap,
            working_set_bytes: self.config.smem_size,
        };
        self.simulate_patterns_timed(vec![vec![run]])
            .into_iter()
            .next()
            .map(|x| x.duration_cycles)
            .unwrap_or(0)
    }

    pub fn simulate_patterns_timed(
        &self,
        streams: Vec<Vec<SmemPatternRun>>,
    ) -> Vec<SmemPatternResult> {
        if streams.iter().all(|stream| stream.is_empty()) {
            return Vec::new();
        }

        let mut graph = FlowGraph::new();
        let mut model = Self::attach(&mut graph, &self.config);
        let mut next_pattern = vec![0usize; streams.len()];
        let mut active: Vec<Option<ActivePatternState>> =
            streams.iter().map(|_| None).collect::<Vec<_>>();
        let mut owner: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
        let mut deferred_completions: VecDeque<SmemCompletion> = VecDeque::new();
        let mut results: Vec<SmemPatternResult> = Vec::new();
        let mut now: Cycle = 0;

        for stream_id in 0..streams.len() {
            activate_next_pattern(&streams, &mut next_pattern, &mut active, stream_id, 0);
        }

        let total_lane_reqs: usize = streams
            .iter()
            .flatten()
            .map(|run| run.addrs.len().saturating_mul(run.active_lanes.max(1)))
            .sum();
        let max_cycles = (total_lane_reqs as u64)
            .saturating_mul(1024)
            .saturating_add(1_000_000)
            .max(4096);

        while now < max_cycles {
            graph.tick(now);
            model.collect_completions(&mut graph, now);

            // RTL TLTrafficGen updates source-id occupancy and outstanding
            // counters in registers. A D beat that fires in cycle N cannot
            // make a new A beat issue in that same cycle.
            while let Some(completion) = deferred_completions.pop_front() {
                if let Some(owners) = owner.remove(&completion.request.id) {
                    for (stream_id, lane) in owners {
                        if let Some(pattern) = active.get_mut(stream_id).and_then(Option::as_mut) {
                            if lane < pattern.inflight.len() {
                                pattern.inflight[lane] = pattern.inflight[lane].saturating_sub(1);
                            }
                            pattern.completed_lane_reqs =
                                pattern.completed_lane_reqs.saturating_add(1);
                        }
                    }
                }
            }

            let mut completed_streams = Vec::new();
            for stream_id in 0..active.len() {
                let Some(pattern) = active[stream_id].as_ref() else {
                    continue;
                };
                let lane_count = lane_count_for_run(&pattern.run, model.config.num_lanes);
                let total = pattern.run.addrs.len().saturating_mul(lane_count);
                let all_issued = (0..lane_count).all(|lane| {
                    pattern.req_idx.get(lane).copied().unwrap_or(0) >= pattern.run.addrs.len()
                });
                let none_inflight = (0..lane_count)
                    .all(|lane| pattern.inflight.get(lane).copied().unwrap_or(0) == 0);
                if total == 0
                    || (all_issued && none_inflight && pattern.completed_lane_reqs >= total)
                {
                    completed_streams.push(stream_id);
                }
            }

            let finished_cycle = now;
            for stream_id in completed_streams {
                if let Some(pattern) = active[stream_id].take() {
                    results.push(SmemPatternResult {
                        stream_id: pattern.run.stream_id,
                        pattern_idx: pattern.run.pattern_idx,
                        suite: pattern.run.suite,
                        pattern_name: pattern.run.pattern_name,
                        finished_cycle,
                        duration_cycles: finished_cycle.saturating_sub(pattern.start_cycle),
                        reqs_per_lane: pattern.run.addrs.len() as u32,
                        active_lanes: pattern.run.active_lanes,
                        issue_gap: pattern.run.issue_gap,
                        max_outstanding: pattern.run.max_inflight_per_lane,
                        working_set_bytes: pattern.run.working_set_bytes,
                    });
                    activate_next_pattern(
                        &streams,
                        &mut next_pattern,
                        &mut active,
                        stream_id,
                        finished_cycle,
                    );
                }
            }

            let all_done = active.iter().all(Option::is_none)
                && next_pattern
                    .iter()
                    .enumerate()
                    .all(|(idx, next)| *next >= streams[idx].len());
            if all_done {
                break;
            }

            for stream_id in 0..active.len() {
                let Some((candidates, req_bytes, is_read, issue_gap)) =
                    active[stream_id].as_ref().map(|pattern| {
                        (
                            lane_issue_candidates(pattern, now, model.config.num_lanes),
                            pattern.run.req_bytes.max(1),
                            pattern.run.is_read,
                            pattern.run.issue_gap,
                        )
                    })
                else {
                    continue;
                };
                if candidates.is_empty() {
                    continue;
                }

                for candidate in candidates {
                    let addr = candidate.addr;
                    let lane = candidate.lane;
                    let (bank, subbank) = model.config.decode_addr(addr);
                    let mut request = SmemRequest::new(stream_id, req_bytes, 1, !is_read, bank);
                    request.addr = addr;
                    request.subbank = subbank;
                    request.client = classify_smem_client(
                        &active[stream_id]
                            .as_ref()
                            .map(|pattern| pattern.run.suite.as_str())
                            .unwrap_or(""),
                    );
                    request.lane_addrs = Some(vec![addr]);
                    request.lane_ids = Some(vec![lane]);
                    match model.issue(&mut graph, now, request) {
                        Ok(issue) => {
                            owner.insert(issue.request_id, vec![(stream_id, lane)]);
                            if let Some(pattern) =
                                active.get_mut(stream_id).and_then(Option::as_mut)
                            {
                                if lane < pattern.req_idx.len() {
                                    pattern.req_idx[lane] = pattern.req_idx[lane].saturating_add(1);
                                }
                                if lane < pattern.inflight.len() {
                                    pattern.inflight[lane] =
                                        pattern.inflight[lane].saturating_add(1);
                                }
                                if lane < pattern.next_issue_at.len() {
                                    pattern.next_issue_at[lane] =
                                        now.saturating_add(issue_gap).saturating_add(1);
                                }
                            }
                        }
                        Err(_) => {}
                    }
                }
            }

            deferred_completions.extend(model.completions.drain(..));
            now = now.saturating_add(1);
        }

        results.sort_by_key(|x| (x.finished_cycle, x.stream_id, x.pattern_idx));
        results
    }

    pub fn issue(
        &mut self,
        _graph: &mut FlowGraph<CoreFlowPayload>,
        now: Cycle,
        mut request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        let assigned_id = if request.id == 0 {
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            id
        } else {
            self.next_id = self.next_id.max(request.id.saturating_add(1));
            request.id
        };
        request.id = assigned_id;

        if self.total_inflight >= self.config.max_outstanding as u64 {
            self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
            return Err(SmemReject::new(
                request,
                now.saturating_add(1),
                SmemRejectReason::QueueFull,
            ));
        }

        let lane_ops = self.build_lane_ops(&request);
        if lane_ops.is_empty() {
            self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
            return Err(SmemReject::new(
                request,
                now.saturating_add(1),
                SmemRejectReason::Busy,
            ));
        }

        if request.client == SmemClientKind::Gemmini {
            let mut bank_demand: HashMap<(bool, usize, usize), usize> = HashMap::new();
            for op in &lane_ops {
                *bank_demand
                    .entry((op.is_store, op.bank, op.subbank))
                    .or_default() += 1;
            }
            for ((is_store, bank, subbank), demand) in bank_demand {
                if self.gemmini_bank_available_slots(is_store, bank, subbank) < demand {
                    self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
                    return Err(SmemReject::new(
                        request,
                        now.saturating_add(1),
                        SmemRejectReason::QueueFull,
                    ));
                }
            }
        } else {
            let mut prealign_demand = vec![0usize; self.prealign.len().max(1)];
            for op in &lane_ops {
                let idx = self.prealign_index(op.lane);
                prealign_demand[idx] = prealign_demand[idx].saturating_add(1);
            }
            for (idx, demand) in prealign_demand.iter().enumerate() {
                if *demand > self.prealign[idx].available_slots() {
                    self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
                    return Err(SmemReject::new(
                        request,
                        now.saturating_add(1),
                        SmemRejectReason::QueueFull,
                    ));
                }
                if let crate::timeq::AcceptStatus::Busy { available_at } =
                    self.prealign[idx].accept_status(now)
                {
                    self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                    return Err(SmemReject::new(
                        request,
                        available_at,
                        SmemRejectReason::Busy,
                    ));
                }
            }
        }

        self.record_bank_pressure(&lane_ops);

        if request.client == SmemClientKind::Gemmini {
            for op in lane_ops.iter().cloned() {
                self.enqueue_bank(now, op)
                    .expect("gemmini bank slots were checked before enqueue");
            }
        } else {
            for op in lane_ops.iter().cloned() {
                let idx = self.prealign_index(op.lane);
                self.prealign[idx]
                    .try_enqueue(now, ServiceRequest::new(op, 1))
                    .expect("prealign slots were checked before enqueue");
            }
        }

        let ops_total = lane_ops.len().max(1);
        self.inflight_requests.insert(
            assigned_id,
            InflightRequestState {
                request: request.clone(),
                pending_ops: VecDeque::new(),
                ops_total,
                ops_completed: 0,
                max_bank_ready_at: now,
                tail_pending: false,
                tail_enqueued: false,
            },
        );

        self.total_inflight = self.total_inflight.saturating_add(1);
        self.stats.issued = self.stats.issued.saturating_add(1);
        if request.is_store {
            self.stats.write_issued = self.stats.write_issued.saturating_add(1);
        } else {
            self.stats.read_issued = self.stats.read_issued.saturating_add(1);
        }
        self.stats.bytes_issued = self.stats.bytes_issued.saturating_add(request.bytes as u64);
        self.stats.inflight = self.total_inflight;
        self.stats.max_inflight = self.stats.max_inflight.max(self.total_inflight);

        Ok(SmemIssue {
            request_id: assigned_id,
            ticket: Ticket::new(now, now, request.bytes),
        })
    }

    pub fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        self.process_prealign(now);
        self.process_xbar(now);
        self.process_banked_array(now);
        self.process_tail(now);

        self.stats.max_completion_queue = self
            .stats
            .max_completion_queue
            .max(self.completions.len() as u64);
    }

    fn process_prealign(&mut self, now: Cycle) {
        if let Some(gate) = self.full_serial_gate.as_mut() {
            for idx in 0..self.prealign.len() {
                loop {
                    let Some(op) = self.prealign[idx]
                        .peek_ready(now)
                        .map(|ready| ready.payload.clone())
                    else {
                        break;
                    };
                    if gate.try_enqueue(now, ServiceRequest::new(op, 1)).is_err() {
                        break;
                    }
                    let _ = self.prealign[idx].pop_ready(now);
                }
            }
        }
        if self.full_serial_gate.is_some() {
            loop {
                let Some(op) = self
                    .full_serial_gate
                    .as_mut()
                    .and_then(|gate| gate.peek_ready(now).map(|ready| ready.payload.clone()))
                else {
                    break;
                };
                if self.enqueue_xbar(op).is_err() {
                    break;
                }
                let _ = self
                    .full_serial_gate
                    .as_mut()
                    .and_then(|gate| gate.pop_ready(now));
            }
            return;
        }

        if matches!(self.config.serialization, SmemSerialization::CoreSerialized) {
            let mut ready: Vec<(usize, LaneOp)> = Vec::new();
            for idx in 0..self.prealign.len() {
                if let Some(op) = self.prealign[idx]
                    .peek_ready(now)
                    .map(|ready| ready.payload.clone())
                {
                    ready.push((idx, op));
                }
            }
            for group in coalesce_ready_lane_ops_with_sources(ready) {
                let op = group.op;
                if self.enqueue_xbar(op).is_ok() {
                    for idx in group.sources {
                        let _ = self.prealign[idx].pop_ready(now);
                    }
                }
            }
        } else {
            for idx in 0..self.prealign.len() {
                loop {
                    let Some(op) = self.prealign[idx]
                        .peek_ready(now)
                        .map(|ready| ready.payload.clone())
                    else {
                        break;
                    };
                    if self.enqueue_xbar(op).is_err() {
                        break;
                    }
                    let _ = self.prealign[idx].pop_ready(now);
                }
            }
        }
    }

    fn xbar_input_index(&self, op: &LaneOp) -> usize {
        match self.config.serialization {
            SmemSerialization::FullySerialized => 0,
            SmemSerialization::CoreSerialized | SmemSerialization::NotSerialized => {
                op.lane.min(self.config.crossbar_inputs().saturating_sub(1))
            }
        }
    }

    fn gemmini_bank_available_slots(&self, is_store: bool, bank: usize, subbank: usize) -> usize {
        let bank = bank.min(self.config.num_banks.max(1).saturating_sub(1));
        let subbank = subbank.min(self.config.active_subbanks().max(1).saturating_sub(1));
        if is_store {
            self.gemmini_bank_write_ports[bank][subbank]
                .iter()
                .map(TimedServer::available_slots)
                .sum()
        } else {
            self.gemmini_bank_read_ports[bank][subbank]
                .iter()
                .map(TimedServer::available_slots)
                .sum()
        }
    }

    fn process_xbar(&mut self, now: Cycle) {
        self.step_xbar_grants(now, false);
        self.step_xbar_grants(now, true);
        for output in 0..self.read_xbar_output_servers.len() {
            loop {
                let Some(op) = self.read_xbar_output_servers[output]
                    .peek_ready(now)
                    .map(|ready| ready.payload.clone())
                else {
                    break;
                };
                if self.enqueue_bank(now, op).is_err() {
                    break;
                }
                let _ = self.read_xbar_output_servers[output].pop_ready(now);
            }
        }
        for output in 0..self.write_xbar_output_servers.len() {
            loop {
                let Some(op) = self.write_xbar_output_servers[output]
                    .peek_ready(now)
                    .map(|ready| ready.payload.clone())
                else {
                    break;
                };
                if self.enqueue_bank(now, op).is_err() {
                    break;
                }
                let _ = self.write_xbar_output_servers[output].pop_ready(now);
            }
        }
    }

    fn enqueue_xbar(&mut self, op: LaneOp) -> Result<(), LaneOp> {
        let input = self.xbar_input_index(&op);
        if op.is_store {
            if input >= self.write_xbar_input_queues.len()
                || self.write_xbar_input_queues[input].len() >= self.config.port_queue_depth.max(1)
            {
                return Err(op);
            }
            self.write_xbar_input_queues[input].push_back(op);
        } else {
            if input >= self.read_xbar_input_queues.len()
                || self.read_xbar_input_queues[input].len() >= self.config.port_queue_depth.max(1)
            {
                return Err(op);
            }
            self.read_xbar_input_queues[input].push_back(op);
        }
        Ok(())
    }

    fn process_banked_array(&mut self, now: Cycle) {
        let num_banks = self.config.num_banks.max(1);
        let num_subbanks = self.config.active_subbanks().max(1);
        for bank in 0..num_banks {
            for subbank in 0..num_subbanks {
                for port in 0..self.bank_read_ports[bank][subbank].len() {
                    self.try_drain_bank_port(now, bank, subbank, port, false, SmemClientKind::Muon);
                }
                for port in 0..self.bank_write_ports[bank][subbank].len() {
                    self.try_drain_bank_port(now, bank, subbank, port, true, SmemClientKind::Muon);
                }
                for port in 0..self.gemmini_bank_read_ports[bank][subbank].len() {
                    self.try_drain_bank_port(
                        now,
                        bank,
                        subbank,
                        port,
                        false,
                        SmemClientKind::Gemmini,
                    );
                }
                for port in 0..self.gemmini_bank_write_ports[bank][subbank].len() {
                    self.try_drain_bank_port(
                        now,
                        bank,
                        subbank,
                        port,
                        true,
                        SmemClientKind::Gemmini,
                    );
                }
            }
        }
    }

    fn enqueue_bank(&mut self, now: Cycle, op: LaneOp) -> Result<(), LaneOp> {
        let bank = op.bank.min(self.config.num_banks.max(1).saturating_sub(1));
        let subbank = op
            .subbank
            .min(self.config.active_subbanks().max(1).saturating_sub(1));
        if op.client == SmemClientKind::Gemmini && op.is_store {
            let ports = &mut self.gemmini_bank_write_ports[bank][subbank];
            let rr = &mut self.gemmini_bank_write_rr_next[bank][subbank];
            if let Some(port) = pick_ready_port(ports, rr, now) {
                ports[port]
                    .try_enqueue(now, ServiceRequest::new(op, 1))
                    .expect("port readiness checked before enqueue");
                self.gemmini_bank_write_inflight[bank] =
                    self.gemmini_bank_write_inflight[bank].saturating_add(1);
                return Ok(());
            }
        } else if op.client == SmemClientKind::Gemmini {
            let ports = &mut self.gemmini_bank_read_ports[bank][subbank];
            let rr = &mut self.gemmini_bank_read_rr_next[bank][subbank];
            if let Some(port) = pick_ready_port(ports, rr, now) {
                ports[port]
                    .try_enqueue(now, ServiceRequest::new(op, 1))
                    .expect("port readiness checked before enqueue");
                self.gemmini_bank_read_inflight[bank] =
                    self.gemmini_bank_read_inflight[bank].saturating_add(1);
                return Ok(());
            }
        } else if op.is_store {
            let ports = &mut self.bank_write_ports[bank][subbank];
            let rr = &mut self.bank_write_rr_next[bank][subbank];
            if let Some(port) = pick_ready_port(ports, rr, now) {
                ports[port]
                    .try_enqueue(now, ServiceRequest::new(op, 1))
                    .expect("port readiness checked before enqueue");
                self.bank_write_inflight[bank] = self.bank_write_inflight[bank].saturating_add(1);
                return Ok(());
            }
        } else {
            let ports = &mut self.bank_read_ports[bank][subbank];
            let rr = &mut self.bank_read_rr_next[bank][subbank];
            if let Some(port) = pick_ready_port(ports, rr, now) {
                ports[port]
                    .try_enqueue(now, ServiceRequest::new(op, 1))
                    .expect("port readiness checked before enqueue");
                self.bank_read_inflight[bank] = self.bank_read_inflight[bank].saturating_add(1);
                return Ok(());
            }
        }
        Err(op)
    }

    fn process_tail(&mut self, now: Cycle) {
        let mut done: Vec<SmemCompletion> = Vec::new();
        for tail in &mut self.read_tail {
            tail.service_ready(now, |result| {
                done.push(result.payload);
            });
        }
        for tail in &mut self.write_tail {
            tail.service_ready(now, |result| {
                done.push(result.payload);
            });
        }
        for completion in done {
            self.finish_request(now, completion);
        }
    }

    fn enqueue_tail(&mut self, now: Cycle, completion: SmemCompletion) -> Result<(), Cycle> {
        let (is_store, idx) = self.tail_key(&completion);
        // Read completions model the SRAM read-data pipe. Write completions are
        // an ack path and must not pay that extra service cycle.
        let service_bytes = if is_store { 0 } else { 1 };
        let enqueue = if is_store {
            self.write_tail[idx].try_enqueue(now, ServiceRequest::new(completion, service_bytes))
        } else {
            self.read_tail[idx].try_enqueue(now, ServiceRequest::new(completion, service_bytes))
        };
        match enqueue {
            Ok(_ticket) => Ok(()),
            Err(bp) => {
                let (retry_at, _reason) = map_backpressure(now, bp);
                Err(retry_at)
            }
        }
    }

    fn tail_key(&self, completion: &SmemCompletion) -> (bool, usize) {
        let lane = completion
            .request
            .lane_ids
            .as_ref()
            .and_then(|lanes| lanes.first().copied())
            .unwrap_or(0);
        if completion.request.is_store {
            (true, lane.min(self.write_tail.len().saturating_sub(1)))
        } else {
            (false, lane.min(self.read_tail.len().saturating_sub(1)))
        }
    }

    fn can_enqueue_tail_batch(&self, now: Cycle, completions: &[SmemCompletion]) -> bool {
        let mut needed: HashMap<(bool, usize), usize> = HashMap::new();
        for completion in completions {
            *needed.entry(self.tail_key(completion)).or_default() += 1;
        }
        for ((is_store, idx), count) in needed {
            let Some(tail) = (if is_store {
                self.write_tail.get(idx)
            } else {
                self.read_tail.get(idx)
            }) else {
                return false;
            };
            if !matches!(tail.accept_status(now), AcceptStatus::Ready) {
                return false;
            }
            if tail.available_slots() < count {
                return false;
            }
        }
        true
    }

    fn step_xbar_grants(&mut self, now: Cycle, is_store: bool) {
        let (queues, output_servers, rr_masks) = if is_store {
            (
                &mut self.write_xbar_input_queues,
                &mut self.write_xbar_output_servers,
                &mut self.write_xbar_rr_masks,
            )
        } else {
            (
                &mut self.read_xbar_input_queues,
                &mut self.read_xbar_output_servers,
                &mut self.read_xbar_rr_masks,
            )
        };
        let num_inputs = queues.len();
        if num_inputs == 0 {
            return;
        }

        for output in 0..output_servers.len() {
            if !matches!(
                output_servers[output].accept_status(now),
                AcceptStatus::Ready
            ) {
                continue;
            }

            let mut best_priority = usize::MAX;
            for input in 0..num_inputs.min(32) {
                let Some(front) = queues[input].front() else {
                    continue;
                };
                if self
                    .config
                    .xbar_output_for_target(front.bank, front.subbank)
                    == output
                {
                    best_priority = best_priority.min(client_priority(front.client));
                }
            }
            if best_priority == usize::MAX {
                continue;
            }

            let mut valid_bits = 0u32;
            for input in 0..num_inputs.min(32) {
                let Some(front) = queues[input].front() else {
                    continue;
                };
                if self
                    .config
                    .xbar_output_for_target(front.bank, front.subbank)
                    == output
                    && client_priority(front.client) == best_priority
                {
                    valid_bits |= 1u32 << input;
                }
            }
            if valid_bits == 0 {
                continue;
            }

            let (winner, new_mask) =
                chisel_rr_winner(valid_bits, rr_masks[output], num_inputs.min(32));
            rr_masks[output] = new_mask;
            if let Some(op) = queues[winner].pop_front() {
                let size_bytes = op.bytes.max(1);
                let _ =
                    output_servers[output].try_enqueue(now, ServiceRequest::new(op, size_bytes));
            }
        }
    }

    fn try_drain_bank_port(
        &mut self,
        now: Cycle,
        bank: usize,
        subbank: usize,
        port: usize,
        is_store: bool,
        client: SmemClientKind,
    ) {
        let ready = match (client, is_store) {
            (SmemClientKind::Gemmini, true) => self.gemmini_bank_write_ports[bank][subbank][port]
                .peek_ready(now)
                .map(|result| (result.payload.clone(), result.ticket.ready_at())),
            (SmemClientKind::Gemmini, false) => self.gemmini_bank_read_ports[bank][subbank][port]
                .peek_ready(now)
                .map(|result| (result.payload.clone(), result.ticket.ready_at())),
            (_, true) => self.bank_write_ports[bank][subbank][port]
                .peek_ready(now)
                .map(|result| (result.payload.clone(), result.ticket.ready_at())),
            (_, false) => self.bank_read_ports[bank][subbank][port]
                .peek_ready(now)
                .map(|result| (result.payload.clone(), result.ticket.ready_at())),
        };
        let Some((op, bank_ready_at)) = ready else {
            return;
        };

        let request_counts = op.request_completion_counts();
        let mut completions = Vec::new();
        for (request_id, completed_ops) in &request_counts {
            let Some(state) = self.inflight_requests.get(request_id) else {
                continue;
            };
            let completes_request =
                state.ops_completed.saturating_add(*completed_ops) >= state.ops_total;
            if completes_request && !state.tail_enqueued {
                let ready_at = state.max_bank_ready_at.max(bank_ready_at);
                completions.push(SmemCompletion {
                    request: state.request.clone(),
                    ticket_ready_at: ready_at,
                    completed_at: ready_at,
                });
            }
        }

        if !self.can_enqueue_tail_batch(now, &completions) {
            return;
        }

        for completion in completions {
            let _ = self.enqueue_tail(now, completion);
        }

        match (client, is_store) {
            (SmemClientKind::Gemmini, true) => {
                let _ = self.gemmini_bank_write_ports[bank][subbank][port].pop_ready(now);
                self.gemmini_bank_write_inflight[bank] =
                    self.gemmini_bank_write_inflight[bank].saturating_sub(1);
            }
            (SmemClientKind::Gemmini, false) => {
                let _ = self.gemmini_bank_read_ports[bank][subbank][port].pop_ready(now);
                self.gemmini_bank_read_inflight[bank] =
                    self.gemmini_bank_read_inflight[bank].saturating_sub(1);
            }
            (_, true) => {
                let _ = self.bank_write_ports[bank][subbank][port].pop_ready(now);
                self.bank_write_inflight[bank] = self.bank_write_inflight[bank].saturating_sub(1);
            }
            (_, false) => {
                let _ = self.bank_read_ports[bank][subbank][port].pop_ready(now);
                self.bank_read_inflight[bank] = self.bank_read_inflight[bank].saturating_sub(1);
            }
        }

        for (request_id, completed_ops) in request_counts {
            let Some(state) = self.inflight_requests.get_mut(&request_id) else {
                continue;
            };
            state.ops_completed = state.ops_completed.saturating_add(completed_ops);
            state.max_bank_ready_at = state.max_bank_ready_at.max(bank_ready_at);
            if state.ops_completed >= state.ops_total {
                state.tail_pending = false;
                state.tail_enqueued = true;
            }
        }
    }

    fn finish_request(&mut self, now: Cycle, mut completion: SmemCompletion) {
        completion.completed_at = now;
        let is_store = completion.request.is_store;
        let bytes = completion.request.bytes as u64;
        self.stats.completed = self.stats.completed.saturating_add(1);
        if is_store {
            self.stats.write_completed = self.stats.write_completed.saturating_add(1);
        } else {
            self.stats.read_completed = self.stats.read_completed.saturating_add(1);
        }
        self.stats.bytes_completed = self.stats.bytes_completed.saturating_add(bytes);
        self.total_inflight = self.total_inflight.saturating_sub(1);
        self.stats.inflight = self.total_inflight;
        self.stats.last_completion_cycle = Some(now);
        self.inflight_requests.remove(&completion.request.id);
        self.completions.push_back(completion);
    }

    fn build_lane_ops(&self, request: &SmemRequest) -> Vec<LaneOp> {
        let lane_targets = self.collect_lane_targets(request);
        if lane_targets.is_empty() {
            return Vec::new();
        }
        let bytes_per_lane = request
            .bytes
            .saturating_div(lane_targets.len().max(1) as u32)
            .max(1);
        let word_bytes = self.config.word_bytes.max(1);

        let split_wide_single_lane = request
            .lane_addrs
            .as_ref()
            .map(|addrs| addrs.len() == 1 && bytes_per_lane > word_bytes)
            .unwrap_or(false);
        if !split_wide_single_lane {
            return lane_targets
                .into_iter()
                .map(|(lane, addr)| {
                    let aligned_addr = align_down(addr, word_bytes as u64);
                    let (bank, subbank) = self.config.decode_addr(aligned_addr);
                    LaneOp {
                        request_id: request.id,
                        request_ids: vec![request.id],
                        lane,
                        addr: aligned_addr,
                        bytes: bytes_per_lane.max(word_bytes),
                        coalesced_ops: 1,
                        bank,
                        subbank,
                        is_store: request.is_store,
                        client: request.client,
                    }
                })
                .collect();
        }

        let mut ops = Vec::new();
        for (lane, base_addr) in lane_targets {
            let start = align_down(base_addr, word_bytes as u64);
            let end = align_up(
                base_addr.saturating_add(bytes_per_lane as u64),
                word_bytes as u64,
            );
            let mut addr = start;
            let mut word_idx = 0usize;
            while addr < end {
                let (bank, subbank) = self.config.decode_addr(addr);
                ops.push(LaneOp {
                    request_id: request.id,
                    request_ids: vec![request.id],
                    lane: (lane + word_idx).min(self.config.num_lanes.max(1).saturating_sub(1)),
                    addr,
                    bytes: word_bytes,
                    coalesced_ops: 1,
                    bank,
                    subbank,
                    is_store: request.is_store,
                    client: request.client,
                });
                addr = addr.saturating_add(word_bytes as u64);
                word_idx = word_idx.saturating_add(1);
            }
        }
        ops
    }

    fn collect_lane_targets(&self, request: &SmemRequest) -> Vec<(usize, u64)> {
        if let Some(lane_addrs) = request.lane_addrs.as_ref() {
            if !lane_addrs.is_empty() {
                let active = self.active_lane_count(request);
                return lane_addrs
                    .iter()
                    .copied()
                    .take(active)
                    .enumerate()
                    .map(|(idx, addr)| {
                        let lane = request
                            .lane_ids
                            .as_ref()
                            .and_then(|lanes| lanes.get(idx).copied())
                            .unwrap_or(idx);
                        (
                            lane.min(self.config.num_lanes.max(1).saturating_sub(1)),
                            addr,
                        )
                    })
                    .collect();
            }
        }
        let active = self.active_lane_count(request);
        (0..active)
            .map(|lane| (lane, request.addr))
            .collect::<Vec<_>>()
    }

    fn active_lane_count(&self, request: &SmemRequest) -> usize {
        let max_lanes = self.config.num_lanes.max(1);
        if let Some(lane_addrs) = request.lane_addrs.as_ref() {
            if !lane_addrs.is_empty() {
                return lane_addrs.len().min(max_lanes);
            }
        }
        let explicit = request.active_lanes as usize;
        if explicit == 0 {
            max_lanes
        } else {
            explicit.min(max_lanes)
        }
    }

    fn record_bank_pressure(&mut self, ops: &[LaneOp]) {
        let num_banks = self.config.num_banks.max(1);
        let mut by_bank = vec![0u64; num_banks];
        for op in ops {
            by_bank[op.bank] = by_bank[op.bank].saturating_add(1);
        }
        for bank in 0..num_banks {
            let attempts = by_bank[bank];
            if attempts == 0 {
                continue;
            }
            self.stats.bank_attempts[bank] =
                self.stats.bank_attempts[bank].saturating_add(attempts);
            let conflicts = attempts.saturating_sub(1);
            self.stats.bank_conflicts[bank] =
                self.stats.bank_conflicts[bank].saturating_add(conflicts);
        }
    }

    fn busy_snapshot(&self) -> (usize, usize, Vec<bool>, Vec<bool>) {
        let num_banks = self.config.num_banks.max(1);

        let mut lane_busy: usize = self.prealign.iter().map(TimedServer::outstanding).sum();
        if let Some(gate) = self.full_serial_gate.as_ref() {
            lane_busy = lane_busy.saturating_add(gate.outstanding());
        }
        lane_busy = lane_busy
            .saturating_add(self.xbar_pending_total())
            .saturating_add(
                self.read_xbar_output_servers
                    .iter()
                    .map(TimedServer::outstanding)
                    .sum(),
            )
            .saturating_add(
                self.write_xbar_output_servers
                    .iter()
                    .map(TimedServer::outstanding)
                    .sum(),
            )
            .saturating_add(self.read_tail.iter().map(TimedServer::outstanding).sum())
            .saturating_add(self.write_tail.iter().map(TimedServer::outstanding).sum());

        let waiting_ops: usize = self
            .inflight_requests
            .values()
            .map(|state| state.pending_ops.len())
            .sum();
        lane_busy = lane_busy.saturating_add(waiting_ops);

        let in_bank: usize = self
            .bank_read_inflight
            .iter()
            .chain(self.bank_write_inflight.iter())
            .chain(self.gemmini_bank_read_inflight.iter())
            .chain(self.gemmini_bank_write_inflight.iter())
            .map(|v| *v as usize)
            .sum();
        lane_busy = lane_busy.saturating_add(in_bank);

        let mut read_busy_banks = vec![false; num_banks];
        let mut write_busy_banks = vec![false; num_banks];
        for bank in 0..num_banks {
            read_busy_banks[bank] =
                self.bank_read_inflight[bank] > 0 || self.gemmini_bank_read_inflight[bank] > 0;
            write_busy_banks[bank] =
                self.bank_write_inflight[bank] > 0 || self.gemmini_bank_write_inflight[bank] > 0;
        }
        let bank_busy = read_busy_banks
            .iter()
            .zip(write_busy_banks.iter())
            .filter(|(r, w)| **r || **w)
            .count();

        (lane_busy, bank_busy, read_busy_banks, write_busy_banks)
    }

    fn prealign_index(&self, lane: usize) -> usize {
        lane.min(self.prealign.len().saturating_sub(1))
    }

    fn xbar_pending_total(&self) -> usize {
        self.read_xbar_input_queues
            .iter()
            .chain(self.write_xbar_input_queues.iter())
            .map(VecDeque::len)
            .sum()
    }
}

fn activate_next_pattern(
    streams: &[Vec<SmemPatternRun>],
    next_pattern: &mut [usize],
    active: &mut [Option<ActivePatternState>],
    stream_id: usize,
    start_cycle: Cycle,
) {
    if stream_id >= streams.len() || next_pattern[stream_id] >= streams[stream_id].len() {
        return;
    }
    let run = streams[stream_id][next_pattern[stream_id]].clone();
    let lane_count = lane_count_for_run(&run, usize::MAX);
    next_pattern[stream_id] = next_pattern[stream_id].saturating_add(1);
    active[stream_id] = Some(ActivePatternState {
        run,
        start_cycle,
        req_idx: vec![0; lane_count],
        inflight: vec![0; lane_count],
        next_issue_at: vec![start_cycle; lane_count],
        completed_lane_reqs: 0,
    });
}

fn lane_count_for_run(run: &SmemPatternRun, config_num_lanes: usize) -> usize {
    let available_lanes = run
        .addrs
        .first()
        .map(|row| row.len())
        .unwrap_or(run.active_lanes)
        .max(1);
    run.active_lanes
        .max(1)
        .min(available_lanes)
        .min(config_num_lanes.max(1))
}

fn lane_issue_candidates(
    pattern: &ActivePatternState,
    now: Cycle,
    config_num_lanes: usize,
) -> Vec<LaneIssueCandidate> {
    let lane_count = lane_count_for_run(&pattern.run, config_num_lanes);
    let max_inflight = pattern.run.max_inflight_per_lane.max(1);
    let mut candidates = Vec::new();

    for lane in 0..lane_count {
        let req_idx = pattern.req_idx.get(lane).copied().unwrap_or(0);
        if req_idx >= pattern.run.addrs.len() {
            continue;
        }
        if pattern.inflight.get(lane).copied().unwrap_or(0) >= max_inflight {
            continue;
        }
        if now
            < pattern
                .next_issue_at
                .get(lane)
                .copied()
                .unwrap_or(pattern.start_cycle)
        {
            continue;
        }
        let Some(addr) = pattern
            .run
            .addrs
            .get(req_idx)
            .and_then(|row| row.get(lane))
            .copied()
        else {
            continue;
        };
        candidates.push(LaneIssueCandidate { lane, addr });
    }

    candidates
}

fn coalesce_ready_lane_ops_with_sources(ops: Vec<(usize, LaneOp)>) -> Vec<CoalescedReadyGroup> {
    let Some((first_source, first_op)) = ops.first().cloned() else {
        return Vec::new();
    };
    let all_match = ops.iter().all(|(_, op)| {
        op.addr == first_op.addr
            && op.bytes == first_op.bytes
            && op.is_store == first_op.is_store
            && op.bank == first_op.bank
            && op.subbank == first_op.subbank
    });
    if !all_match {
        return ops
            .into_iter()
            .map(|(source, op)| CoalescedReadyGroup {
                op,
                sources: vec![source],
            })
            .collect();
    }

    let mut coalesced = first_op;
    let mut sources = vec![first_source];
    for (source, op) in ops.into_iter().skip(1) {
        coalesced.coalesced_ops = coalesced
            .coalesced_ops
            .saturating_add(op.coalesced_ops.max(1));
        if op.request_ids.is_empty() {
            coalesced.request_ids.push(op.request_id);
        } else {
            coalesced.request_ids.extend(op.request_ids);
        }
        sources.push(source);
    }
    vec![CoalescedReadyGroup {
        op: coalesced,
        sources,
    }]
}

fn classify_smem_client(suite: &str) -> SmemClientKind {
    if suite.contains("gemmini") {
        SmemClientKind::Gemmini
    } else {
        SmemClientKind::Muon
    }
}

fn client_priority(client: SmemClientKind) -> usize {
    match client {
        // RTL uniform subbank xbars connect Gemmini-style nodes before Muon
        // aligned nodes and use lowestIndexFirst arbitration.
        SmemClientKind::Gemmini => 0,
        SmemClientKind::Muon => 1,
        SmemClientKind::External => 2,
    }
}

fn align_down(value: u64, alignment: u64) -> u64 {
    let alignment = alignment.max(1);
    value / alignment * alignment
}

fn align_up(value: u64, alignment: u64) -> u64 {
    let alignment = alignment.max(1);
    value.saturating_add(alignment.saturating_sub(1)) / alignment * alignment
}

fn xbar_rr_initial_mask(num_inputs: usize) -> u32 {
    if num_inputs >= 32 {
        u32::MAX
    } else {
        (1u32 << num_inputs.max(1)) - 1
    }
}

fn chisel_rr_winner(valid_bits: u32, mask: u32, n_inputs: usize) -> (usize, u32) {
    let n = n_inputs.clamp(1, 32);
    let all_bits = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
    let valid = valid_bits & all_bits;
    let filter_val = (((valid & !mask & all_bits) as u64) << n) | valid as u64;

    let mut right_or = filter_val;
    let mut shift = 1u64;
    while shift < (2 * n) as u64 {
        right_or |= right_or >> shift;
        shift <<= 1;
    }
    let double_mask = if n == 32 {
        u64::MAX
    } else {
        (1u64 << (2 * n)) - 1
    };
    right_or &= double_mask;
    let unready = (right_or >> 1) | ((mask as u64) << n);
    let upper = ((unready >> n) & all_bits as u64) as u32;
    let lower = (unready & all_bits as u64) as u32;
    let readys = !(upper & lower) & all_bits;
    let grant = readys & valid;
    if grant == 0 {
        return (0, mask);
    }

    let winner = grant.trailing_zeros() as usize;
    let mut left_mask = grant as u64;
    let mut s = 1u64;
    while s < n as u64 {
        left_mask |= left_mask << s;
        s <<= 1;
    }
    (winner, (left_mask as u32) & all_bits)
}

fn pick_ready_port<T>(ports: &[TimedServer<T>], rr_next: &mut usize, now: Cycle) -> Option<usize> {
    if ports.is_empty() {
        return None;
    }
    let start = *rr_next % ports.len();
    for offset in 0..ports.len() {
        let idx = (start + offset) % ports.len();
        if matches!(ports[idx].accept_status(now), AcceptStatus::Ready) {
            *rr_next = (idx + 1) % ports.len();
            return Some(idx);
        }
    }
    None
}

fn map_backpressure<T>(now: Cycle, backpressure: Backpressure<T>) -> (Cycle, SmemRejectReason) {
    match backpressure {
        Backpressure::QueueFull { .. } => (now.saturating_add(1), SmemRejectReason::QueueFull),
        Backpressure::Busy { available_at, .. } => (
            available_at.max(now.saturating_add(1)),
            SmemRejectReason::Busy,
        ),
    }
}

pub fn extract_smem_request(request: crate::timeq::ServiceRequest<CoreFlowPayload>) -> SmemRequest {
    match request.payload {
        CoreFlowPayload::Smem(req) => req,
        _ => panic!("expected smem request"),
    }
}
