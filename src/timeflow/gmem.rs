use std::collections::VecDeque;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone, Copy, Default)]
pub struct GmemStats {
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
pub struct GmemRequest {
    pub id: u64,
    pub warp: usize,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_load: bool,
    pub stall_on_completion: bool,
}

impl GmemRequest {
    pub fn new(warp: usize, bytes: u32, active_lanes: u32, is_load: bool) -> Self {
        Self {
            id: 0,
            warp,
            bytes,
            active_lanes,
            is_load,
            stall_on_completion: is_load,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GmemCompletion {
    pub request: GmemRequest,
    pub ticket_ready_at: Cycle,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone)]
pub struct GmemIssue {
    pub request_id: u64,
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmemRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct GmemReject {
    pub request: GmemRequest,
    pub retry_at: Cycle,
    pub reason: GmemRejectReason,
}

#[derive(Debug, Clone)]
pub struct GmemFlowConfig {
    pub coalescer: ServerConfig,
    pub cache: ServerConfig,
    pub dram: ServerConfig,
    pub link_capacity: usize,
}

impl Default for GmemFlowConfig {
    fn default() -> Self {
        Self {
            coalescer: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 16,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            cache: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 32,
                ..ServerConfig::default()
            },
            dram: ServerConfig {
                base_latency: 80,
                bytes_per_cycle: 16,
                queue_capacity: 64,
                ..ServerConfig::default()
            },
            link_capacity: 16,
        }
    }
}

pub(crate) struct GmemSubgraph {
    ingress_node: NodeId,
    dram_node: NodeId,
    pub(crate) completions: VecDeque<GmemCompletion>,
    next_id: u64,
    pub(crate) stats: GmemStats,
}

impl GmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &GmemFlowConfig) -> Self {
        let ingress_node = graph.add_node(ServerNode::new(
            "coalescer",
            TimedServer::new(config.coalescer),
        ));
        let cache_node = graph.add_node(ServerNode::new("l2", TimedServer::new(config.cache)));
        let dram_node = graph.add_node(ServerNode::new("dram", TimedServer::new(config.dram)));

        graph.connect(
            ingress_node,
            cache_node,
            "coalescer->l2",
            Link::new(config.link_capacity),
        );
        graph.connect(
            cache_node,
            dram_node,
            "l2->dram",
            Link::new(config.link_capacity),
        );

        Self {
            ingress_node,
            dram_node,
            completions: VecDeque::new(),
            next_id: 0,
            stats: GmemStats::default(),
        }
    }

    pub fn issue(
        &mut self,
        graph: &mut FlowGraph<CoreFlowPayload>,
        now: Cycle,
        mut request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
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
        let payload = CoreFlowPayload::Gmem(request);
        let service_req = ServiceRequest::new(payload, bytes);

        match graph.try_put(self.ingress_node, now, service_req) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                self.stats.bytes_issued = self.stats.bytes_issued.saturating_add(bytes as u64);
                self.stats.inflight = self.stats.inflight.saturating_add(1);
                self.stats.max_inflight = self.stats.max_inflight.max(self.stats.inflight);
                Ok(GmemIssue {
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
                    let request = extract_gmem_request(request);
                    Err(GmemReject {
                        request,
                        retry_at,
                        reason: GmemRejectReason::Busy,
                    })
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.queue_full_rejects += 1;
                    let retry_at = now.saturating_add(1);
                    let request = extract_gmem_request(request);
                    Err(GmemReject {
                        request,
                        retry_at,
                        reason: GmemRejectReason::QueueFull,
                    })
                }
            },
        }
    }

    pub fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        let completions = &mut self.completions;
        graph.with_node_mut(self.dram_node, |node| {
            while let Some(result) = node.take_ready(now) {
                match result.payload {
                    CoreFlowPayload::Gmem(request) => {
                        self.stats.completed = self.stats.completed.saturating_add(1);
                        self.stats.bytes_completed = self
                            .stats
                            .bytes_completed
                            .saturating_add(request.bytes as u64);
                        self.stats.inflight = self.stats.inflight.saturating_sub(1);
                        self.stats.last_completion_cycle = Some(now);
                        completions.push_back(GmemCompletion {
                            ticket_ready_at: result.ticket.ready_at(),
                            completed_at: now,
                            request,
                        });
                    }
                    CoreFlowPayload::Smem(_) => continue,
                }
            }
        });
        self.stats.max_completion_queue = self
            .stats
            .max_completion_queue
            .max(self.completions.len() as u64);
    }
}

pub fn extract_gmem_request(request: ServiceRequest<CoreFlowPayload>) -> GmemRequest {
    match request.payload {
        CoreFlowPayload::Gmem(req) => req,
        _ => panic!("expected gmem request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeflow::graph::FlowGraph;
    use crate::timeflow::types::CoreFlowPayload;

    #[test]
    fn gmem_subgraph_accepts_and_completes() {
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = GmemSubgraph::attach(&mut graph, &GmemFlowConfig::default());
        let now = 0;
        let req = GmemRequest::new(0, 16, 0xF, true);
        let issue = subgraph
            .issue(&mut graph, now, req)
            .expect("issue should succeed");
        let ready_at = issue.ticket.ready_at();
        for cycle in now..=ready_at.saturating_add(200) {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
        }
        assert_eq!(1, subgraph.completions.len());
    }
}
