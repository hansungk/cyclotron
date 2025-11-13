use std::collections::VecDeque;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone, Copy, Default)]
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

#[derive(Debug, Clone)]
pub struct SmemRequest {
    pub id: u64,
    pub warp: usize,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_store: bool,
    pub bank: usize,
}

impl SmemRequest {
    pub fn new(warp: usize, bytes: u32, active_lanes: u32, is_store: bool, bank: usize) -> Self {
        Self {
            id: 0,
            warp,
            bytes,
            active_lanes,
            is_store,
            bank,
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

#[derive(Debug, Clone)]
pub struct SmemFlowConfig {
    pub crossbar: ServerConfig,
    pub bank: ServerConfig,
    pub num_banks: usize,
    pub link_capacity: usize,
}

impl Default for SmemFlowConfig {
    fn default() -> Self {
        Self {
            crossbar: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 32,
                queue_capacity: 32,
                ..ServerConfig::default()
            },
            bank: ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 32,
                ..ServerConfig::default()
            },
            num_banks: 1,
            link_capacity: 32,
        }
    }
}

pub(crate) struct SmemSubgraph {
    ingress_node: NodeId,
    bank_nodes: Vec<NodeId>,
    pub(crate) completions: VecDeque<SmemCompletion>,
    next_id: u64,
    pub(crate) stats: SmemStats,
}

impl SmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
        assert!(config.num_banks > 0, "SMEM must have at least one bank");
        let ingress_node = graph.add_node(ServerNode::new(
            "smem_crossbar",
            TimedServer::new(config.crossbar),
        ));

        let mut bank_nodes = Vec::with_capacity(config.num_banks);
        for bank_idx in 0..config.num_banks {
            let bank_mod = bank_idx;
            let num_banks = config.num_banks;
            let node = graph.add_node(ServerNode::new(
                format!("smem_bank_{bank_idx}"),
                TimedServer::new(config.bank),
            ));
            graph.connect_filtered(
                ingress_node,
                node,
                format!("smem_xbar->bank_{bank_idx}"),
                Link::new(config.link_capacity),
                move |payload| match payload {
                    CoreFlowPayload::Smem(req) => (req.bank % num_banks) == bank_mod,
                    _ => false,
                },
            );
            bank_nodes.push(node);
        }

        Self {
            ingress_node,
            bank_nodes,
            completions: VecDeque::new(),
            next_id: 0,
            stats: SmemStats::default(),
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

        let bytes = request.bytes;
        let payload = CoreFlowPayload::Smem(request);
        let service_req = ServiceRequest::new(payload, bytes);

        match graph.try_put(self.ingress_node, now, service_req) {
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
}
