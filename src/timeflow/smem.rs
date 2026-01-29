use std::collections::VecDeque;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmemRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct SmemReject {
    pub request: SmemRequest,
    pub retry_at: Cycle,
    pub reason: SmemRejectReason,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SmemFlowConfig {
    #[serde(default)]
    pub lane: ServerConfig,
    #[serde(default)]
    pub serial: ServerConfig,
    #[serde(default)]
    pub crossbar: ServerConfig,
    #[serde(default)]
    pub subbank: ServerConfig,
    #[serde(default)]
    pub bank: ServerConfig,
    #[serde(default)]
    pub dual_port: bool,
    #[serde(default)]
    pub num_banks: usize,
    #[serde(default)]
    pub num_lanes: usize,
    #[serde(default)]
    pub num_subbanks: usize,
    #[serde(default)]
    pub word_bytes: u32,
    #[serde(default)]
    pub serialize_cores: bool,
    #[serde(default)]
    pub link_capacity: usize,
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
    subbank_nodes: Vec<Vec<NodeId>>,
    pub(crate) completions: VecDeque<SmemCompletion>,
    next_id: u64,
    pub(crate) stats: SmemStats,
}

impl SmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
        assert!(config.num_banks > 0, "SMEM must have at least one bank");
        assert!(config.num_lanes > 0, "SMEM must have at least one lane");

        let mut lane_nodes = Vec::with_capacity(config.num_lanes);
        for lane_idx in 0..config.num_lanes {
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
            let num_lanes = config.num_lanes.max(1);
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

        let mut crossbar_nodes = Vec::with_capacity(config.num_banks);
        for bank_idx in 0..config.num_banks {
            let node = graph.add_node(ServerNode::new(
                format!("smem_xbar_bank_{bank_idx}"),
                TimedServer::new(config.crossbar),
            ));
            crossbar_nodes.push(node);
        }

        let mut bank_nodes = Vec::with_capacity(config.num_banks);
        let mut bank_read_nodes = Vec::with_capacity(config.num_banks);
        let mut bank_write_nodes = Vec::with_capacity(config.num_banks);
        let mut subbank_nodes = Vec::with_capacity(config.num_banks);
        for bank_idx in 0..config.num_banks {
            let bank_mod = bank_idx;
            let num_banks = config.num_banks;
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

            let mut bank_subbanks = Vec::with_capacity(config.num_subbanks.max(1));
            for subbank_idx in 0..config.num_subbanks.max(1) {
                let subbank_mod = subbank_idx;
                let num_subbanks = config.num_subbanks.max(1);
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
            subbank_nodes.push(bank_subbanks);
        }

        Self {
            serial_node,
            lane_nodes,
            bank_nodes,
            bank_read_nodes,
            bank_write_nodes,
            dual_port: config.dual_port,
            subbank_nodes,
            completions: VecDeque::new(),
            next_id: 0,
            stats: SmemStats::default(),
        }
    }

    pub fn sample_utilization(
        &self,
        graph: &mut FlowGraph<CoreFlowPayload>,
    ) -> SmemUtilSample {
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
                Backpressure::Busy {
                    request,
                    available_at,
                } => {
                    self.stats.busy_rejects += 1;
                    let mut retry_at = available_at;
                    if retry_at <= now {
                        retry_at = now.saturating_add(1);
                    }
                    let request = extract_smem_request(request);
                    Err(SmemReject {
                        request,
                        retry_at,
                        reason: SmemRejectReason::Busy,
                    })
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.queue_full_rejects += 1;
                    let retry_at = now.saturating_add(1);
                    let request = extract_smem_request(request);
                    Err(SmemReject {
                        request,
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
}

pub fn extract_smem_request(request: ServiceRequest<CoreFlowPayload>) -> SmemRequest {
    match request.payload {
        CoreFlowPayload::Smem(req) => req,
        _ => panic!("expected smem request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeflow::graph::FlowGraph;
    use crate::timeflow::types::CoreFlowPayload;

    #[test]
    fn smem_requests_complete() {
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &SmemFlowConfig::default());
        let req = SmemRequest::new(0, 32, 0xF, false, 0);
        let issue = subgraph
            .issue(&mut graph, 0, req)
            .expect("smem issue should succeed");
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(10) {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
        }
        assert_eq!(1, subgraph.completions.len());
    }

    #[test]
    fn smem_backpressure_is_reported() {
        let mut cfg = SmemFlowConfig::default();
        cfg.bank.queue_capacity = 1;
        cfg.crossbar.queue_capacity = 1;
        cfg.lane.queue_capacity = 1;
        cfg.num_lanes = 1;
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);
        let req0 = SmemRequest::new(0, 32, 0xF, false, 0);
        subgraph.issue(&mut graph, 0, req0).unwrap();
        let req1 = SmemRequest::new(0, 32, 0xF, false, 0);
        let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
        assert_eq!(SmemRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn smem_stats_track_activity() {
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &SmemFlowConfig::default());
        let req = SmemRequest::new(0, 32, 0xF, false, 0);
        let issue = subgraph.issue(&mut graph, 0, req).unwrap();
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(100) {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
        }
        let stats = subgraph.stats;
        assert_eq!(1, stats.issued);
        assert_eq!(1, stats.completed);
        assert_eq!(32, stats.bytes_issued);
        assert_eq!(32, stats.bytes_completed);
    }

    #[test]
    fn smem_serialization_backpressures_second_request() {
        let mut cfg = SmemFlowConfig::default();
        cfg.serialize_cores = true;
        cfg.serial.queue_capacity = 1;
        cfg.num_lanes = 1;
        cfg.num_banks = 1;
        cfg.num_subbanks = 1;
        cfg.lane.queue_capacity = 4;
        cfg.crossbar.queue_capacity = 4;
        cfg.bank.queue_capacity = 4;
        cfg.subbank.queue_capacity = 4;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);
        let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
        subgraph.issue(&mut graph, 0, req0).unwrap();
        let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
        let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
        assert_eq!(SmemRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn smem_subbank_conflict_serializes() {
        let mut cfg = SmemFlowConfig::default();
        cfg.serialize_cores = false;
        cfg.num_lanes = 1;
        cfg.num_banks = 1;
        cfg.num_subbanks = 2;
        cfg.lane.queue_capacity = 4;
        cfg.crossbar.queue_capacity = 4;
        cfg.subbank.queue_capacity = 1;
        cfg.subbank.base_latency = 1;
        cfg.bank.queue_capacity = 4;
        cfg.bank.base_latency = 0;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

        let mut req0 = SmemRequest::new(0, 16, 0x1, false, 0);
        req0.subbank = 0;
        let mut req1 = SmemRequest::new(1, 16, 0x1, false, 0);
        req1.subbank = 0;

        subgraph.issue(&mut graph, 0, req0).unwrap();
        subgraph.issue(&mut graph, 0, req1).unwrap();

        let mut completions = Vec::new();
        for cycle in 0..20 {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
            while let Some(done) = subgraph.completions.pop_front() {
                completions.push(done);
            }
        }

        assert_eq!(completions.len(), 2);
        assert!(completions[1].completed_at > completions[0].completed_at);
    }

    #[test]
    fn smem_single_bank_configuration() {
        let mut cfg = SmemFlowConfig::default();
        cfg.num_banks = 1;
        cfg.num_lanes = 2;
        cfg.num_subbanks = 1;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

        let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
        let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
        subgraph.issue(&mut graph, 0, req0).unwrap();
        subgraph.issue(&mut graph, 0, req1).unwrap();

        let mut count = 0;
        for cycle in 0..50 {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
            count += subgraph.completions.len();
            subgraph.completions.clear();
            if count == 2 {
                return;
            }
        }
        panic!("expected completions for both requests");
    }

    #[test]
    fn smem_single_lane_configuration() {
        let mut cfg = SmemFlowConfig::default();
        cfg.num_lanes = 1;
        cfg.num_banks = 2;
        cfg.num_subbanks = 1;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

        let req = SmemRequest::new(0, 16, 0x1, false, 0);
        subgraph.issue(&mut graph, 0, req).unwrap();
        for cycle in 0..50 {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
            if !subgraph.completions.is_empty() {
                return;
            }
        }
        panic!("expected completion in single-lane config");
    }

    #[test]
    fn smem_single_subbank_configuration() {
        let mut cfg = SmemFlowConfig::default();
        cfg.num_lanes = 1;
        cfg.num_banks = 2;
        cfg.num_subbanks = 1;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

        let req = SmemRequest::new(0, 16, 0x1, false, 1);
        subgraph.issue(&mut graph, 0, req).unwrap();
        for cycle in 0..50 {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
            if !subgraph.completions.is_empty() {
                return;
            }
        }
        panic!("expected completion in single-subbank config");
    }

    #[test]
    fn smem_address_zero_access() {
        let mut cfg = SmemFlowConfig::default();
        cfg.num_lanes = 1;
        cfg.num_banks = 2;
        cfg.num_subbanks = 2;

        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

        let mut req = SmemRequest::new(0, 16, 0x1, false, 0);
        req.addr = 0;
        subgraph.issue(&mut graph, 0, req).unwrap();
        for cycle in 0..50 {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
            if !subgraph.completions.is_empty() {
                return;
            }
        }
        panic!("expected completion for address 0");
    }
}
