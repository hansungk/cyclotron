use std::collections::VecDeque;

use crate::timeflow::{
    graph::{FlowGraph, Link},
    server_node::ServerNode,
    types::{CoreFlowPayload, NodeId},
};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmemStats {
    pub issued: u64,
    pub completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
    pub bytes_issued: u64,
    pub bytes_completed: u64,
    pub inflight: u64,
    pub max_inflight: u64,
    pub max_completion_queue: u64,
    pub last_completion_cycle: Option<Cycle>,
    // Sampling and conflict counters (per-bank)
    pub sample_cycles: u64,
    pub bank_busy_samples: Vec<u64>,
    pub bank_attempts: Vec<u64>,
    pub bank_conflicts: Vec<u64>,
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

// Centralize the reject reason alias for SMEM.
pub use crate::timeflow::types::RejectReason as SmemRejectReason;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SmemFlowConfig {
    pub lane: ServerConfig,
    pub serial: ServerConfig,
    pub crossbar: ServerConfig,
    pub subbank: ServerConfig,
    pub bank: ServerConfig,
    pub dual_port: bool,
    pub num_banks: usize,
    pub num_lanes: usize,
    pub num_subbanks: usize,
    pub word_bytes: u32,
    pub serialize_cores: bool,
    pub link_capacity: usize,
    // How often (in cycles) to emit SMEM aggregated logs / CSV rows. Default: 1000.
    pub smem_log_period: Cycle,
}

impl Default for SmemFlowConfig {
    fn default() -> Self {
        Self {
            lane: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            serial: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 64,
                queue_capacity: 1,
                ..ServerConfig::default()
            },
            crossbar: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 32,
                queue_capacity: 32,
                ..ServerConfig::default()
            },
            subbank: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 32,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 32,
                ..ServerConfig::default()
            },
            dual_port: false,
            num_banks: 1,
            num_lanes: 1,
            num_subbanks: 1,
            word_bytes: 4,
            serialize_cores: false,
            link_capacity: 32,
            smem_log_period: 1000,
        }
    }
}

pub(crate) struct SmemSubgraph {
    serial_node: Option<NodeId>,
    lane_nodes: Vec<NodeId>,
    bank_nodes: Vec<NodeId>,
    bank_read_nodes: Vec<NodeId>,
    bank_write_nodes: Vec<NodeId>,
    dual_port: bool,
    pub(crate) completions: VecDeque<SmemCompletion>,
    next_id: u64,
    pub(crate) stats: SmemStats,
}

impl SmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
        assert!(config.num_banks > 0, "SMEM must have at least one bank");
        assert!(config.num_lanes > 0, "SMEM must have at least one lane");
        let num_lanes = config.num_lanes;
        let num_banks = config.num_banks;
        let num_subbanks = config.num_subbanks.max(1);

        let mut lane_nodes = Vec::with_capacity(num_lanes);
        for lane_idx in 0..num_lanes {
            let node = graph.add_node(ServerNode::new(
                format!("smem_lane_{lane_idx}"),
                TimedServer::new(config.lane),
            ));
            lane_nodes.push(node);
        }

        let serial_node = if config.serialize_cores {
            Some(graph.add_node(ServerNode::new(
                "smem_serial",
                TimedServer::new(config.serial),
            )))
        } else {
            None
        };
        if let Some(serial) = serial_node {
            for (lane_idx, &lane_node) in lane_nodes.iter().enumerate() {
                let lane_mod = lane_idx;
                graph.connect_filtered(
                    serial,
                    lane_node,
                    format!("smem_serial->lane_{lane_idx}"),
                    Link::new(config.link_capacity),
                    move |payload| match payload {
                        CoreFlowPayload::Smem(req) => (req.warp % num_lanes) == lane_mod,
                        _ => false,
                    },
                );
            }
        }

        let mut crossbar_nodes = Vec::with_capacity(num_banks);
        for bank_idx in 0..num_banks {
            let node = graph.add_node(ServerNode::new(
                format!("smem_xbar_bank_{bank_idx}"),
                TimedServer::new(config.crossbar),
            ));
            crossbar_nodes.push(node);
        }

        let mut bank_nodes = Vec::with_capacity(num_banks);
        let mut bank_read_nodes = Vec::with_capacity(num_banks);
        let mut bank_write_nodes = Vec::with_capacity(num_banks);
        for bank_idx in 0..num_banks {
            let bank_mod = bank_idx;
            for &lane_node in &lane_nodes {
                graph.connect_filtered(
                    lane_node,
                    crossbar_nodes[bank_idx],
                    format!("lane->{bank_idx}"),
                    Link::new(config.link_capacity),
                    move |payload| match payload {
                        CoreFlowPayload::Smem(req) => (req.bank % num_banks) == bank_mod,
                        _ => false,
                    },
                );
            }

            let mut bank_subbanks = Vec::with_capacity(num_subbanks);
            for subbank_idx in 0..num_subbanks {
                let subbank_mod = subbank_idx;
                let num_subbanks = num_subbanks;
                let subbank_node = graph.add_node(ServerNode::new(
                    format!("smem_subbank_{bank_idx}_{subbank_idx}"),
                    TimedServer::new(config.subbank),
                ));
                graph.connect_filtered(
                    crossbar_nodes[bank_idx],
                    subbank_node,
                    format!("smem_xbar_{bank_idx}->subbank_{subbank_idx}"),
                    Link::new(config.link_capacity),
                    move |payload| match payload {
                        CoreFlowPayload::Smem(req) => (req.subbank % num_subbanks) == subbank_mod,
                        _ => false,
                    },
                );
                bank_subbanks.push(subbank_node);
            }

            if config.dual_port {
                let read_node = graph.add_node(ServerNode::new(
                    format!("smem_bank_r_{bank_idx}"),
                    TimedServer::new(config.bank),
                ));
                let write_node = graph.add_node(ServerNode::new(
                    format!("smem_bank_w_{bank_idx}"),
                    TimedServer::new(config.bank),
                ));
                for &subbank_node in &bank_subbanks {
                    graph.connect_filtered(
                        subbank_node,
                        read_node,
                        format!("smem_subbank->bank_r_{bank_idx}"),
                        Link::new(config.link_capacity),
                        |payload| match payload {
                            CoreFlowPayload::Smem(req) => !req.is_store,
                            _ => false,
                        },
                    );
                    graph.connect_filtered(
                        subbank_node,
                        write_node,
                        format!("smem_subbank->bank_w_{bank_idx}"),
                        Link::new(config.link_capacity),
                        |payload| match payload {
                            CoreFlowPayload::Smem(req) => req.is_store,
                            _ => false,
                        },
                    );
                }
                bank_read_nodes.push(read_node);
                bank_write_nodes.push(write_node);
                bank_nodes.push(read_node);
                bank_nodes.push(write_node);
            } else {
                let node = graph.add_node(ServerNode::new(
                    format!("smem_bank_{bank_idx}"),
                    TimedServer::new(config.bank),
                ));
                for &subbank_node in &bank_subbanks {
                    graph.connect(
                        subbank_node,
                        node,
                        format!("smem_subbank->bank_{bank_idx}"),
                        Link::new(config.link_capacity),
                    );
                }
                bank_nodes.push(node);
            }
        }

        let mut stats = SmemStats::default();
        stats.bank_busy_samples = vec![0; num_banks];
        stats.bank_attempts = vec![0; num_banks];
        stats.bank_conflicts = vec![0; num_banks];

        Self {
            serial_node,
            lane_nodes,
            bank_nodes,
            bank_read_nodes,
            bank_write_nodes,
            dual_port: config.dual_port,
            completions: VecDeque::new(),
            next_id: 0,
            stats,
        }
    }

    /// Sample per-bank and per-port busy state and accumulate into statistics.
    /// Call this once per cycle to build utilization counters.
    pub fn sample_and_accumulate(&mut self, graph: &mut FlowGraph<CoreFlowPayload>) {
        self.stats.sample_cycles = self.stats.sample_cycles.saturating_add(1);
        let mut is_busy = |node_id| graph.with_node_mut(node_id, |nd| nd.outstanding() > 0);
        let num_banks = self.stats.bank_busy_samples.len();
        for bank in 0..num_banks {
            let busy = if self.dual_port {
                let r = self.bank_read_nodes.get(bank).map(|&n| is_busy(n)).unwrap_or(false);
                let w = self.bank_write_nodes.get(bank).map(|&n| is_busy(n)).unwrap_or(false);
                r || w
            } else {
                // bank_nodes stores one node per bank when not dual_port
                let node = self.bank_nodes.get(bank).copied();
                node.map(|n| is_busy(n)).unwrap_or(false)
            };
            if busy {
                self.stats.bank_busy_samples[bank] = self.stats.bank_busy_samples[bank].saturating_add(1);
            }
        }
    }

    pub fn sample_utilization(&self, graph: &mut FlowGraph<CoreFlowPayload>) -> SmemUtilSample {
        let mut lane_busy = 0usize;
        for &node in &self.lane_nodes {
            let busy = graph.with_node_mut(node, |n| n.outstanding() > 0);
            if busy {
                lane_busy += 1;
            }
        }

        let mut bank_busy = 0usize;
        for &node in &self.bank_nodes {
            let busy = graph.with_node_mut(node, |n| n.outstanding() > 0);
            if busy {
                bank_busy += 1;
            }
        }

        SmemUtilSample {
            lane_busy,
            lane_total: self.lane_nodes.len(),
            bank_busy,
            bank_total: self.bank_nodes.len(),
        }
    }

    pub fn issue(
        &mut self,
        graph: &mut FlowGraph<CoreFlowPayload>,
        now: Cycle,
        mut request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        let assigned_id = if request.id == 0 {
            let id = self.next_id;
            self.next_id += 1;
            id
        } else {
            self.next_id = self.next_id.max(request.id + 1);
            request.id
        };
        request.id = assigned_id;

        let lane_idx = request.warp % self.lane_nodes.len();
        let bytes = request.bytes;
        let payload = CoreFlowPayload::Smem(request);
        let service_req = ServiceRequest::new(payload, bytes);
        let ingress_node = if let Some(serial) = self.serial_node {
            serial
        } else {
            self.lane_nodes[lane_idx]
        };
        match graph.try_put(ingress_node, now, service_req) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                self.stats.bytes_issued = self.stats.bytes_issued.saturating_add(bytes as u64);
                self.stats.inflight = self.stats.inflight.saturating_add(1);
                self.stats.max_inflight = self.stats.max_inflight.max(self.stats.inflight);
                Ok(SmemIssue {
                    request_id: assigned_id,
                    ticket,
                })
            }
            Err(bp) => match bp {
                Backpressure::Busy { request, available_at, } => {
                    self.stats.busy_rejects += 1;
                    let retry_at = crate::timeq::normalize_retry(now, available_at);
                    let request = extract_smem_request(request);
                    self.record_bank_attempt_and_conflict(request.bank);
                    Err(SmemReject { payload: request, retry_at, reason: SmemRejectReason::Busy })
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.queue_full_rejects += 1;
                    let retry_at = now.saturating_add(1);
                    let request = extract_smem_request(request);
                    self.record_bank_attempt_and_conflict(request.bank);
                    Err(SmemReject {
                        payload: request,
                        retry_at,
                        reason: SmemRejectReason::QueueFull,
                    })
                }
            },
        }
    }

    pub fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        for &bank_node in &self.bank_nodes {
            graph.with_node_mut(bank_node, |node| {
                while let Some(result) = node.take_ready(now) {
                    match result.payload {
                        CoreFlowPayload::Smem(request) => {
                            self.stats.completed = self.stats.completed.saturating_add(1);
                            self.stats.bytes_completed = self
                                .stats
                                .bytes_completed
                                .saturating_add(request.bytes as u64);
                            self.stats.inflight = self.stats.inflight.saturating_sub(1);
                            self.stats.last_completion_cycle = Some(now);
                            self.completions.push_back(SmemCompletion {
                                ticket_ready_at: result.ticket.ready_at(),
                                completed_at: now,
                                request,
                            });
                        }
                        _ => {}
                    }
                }
            });
        }
        self.stats.max_completion_queue = self
            .stats
            .max_completion_queue
            .max(self.completions.len() as u64);
    }

    fn record_bank_attempt_and_conflict(&mut self, bank: usize) {
        if self.stats.bank_attempts.is_empty() {
            return;
        }
        let bank = bank.min(self.stats.bank_attempts.len().saturating_sub(1));
        self.stats.bank_attempts[bank] = self.stats.bank_attempts[bank].saturating_add(1);
        self.stats.bank_conflicts[bank] = self.stats.bank_conflicts[bank].saturating_add(1);
    }
}

pub fn extract_smem_request(request: ServiceRequest<CoreFlowPayload>) -> SmemRequest {
    match request.payload {
        CoreFlowPayload::Smem(req) => req,
        _ => panic!("expected smem request"),
    }
}

