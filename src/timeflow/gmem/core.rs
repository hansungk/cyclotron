use std::collections::VecDeque;

use crate::timeflow::graph::FlowGraph;
use crate::timeflow::types::CoreFlowPayload;
use crate::timeq::{Backpressure, Cycle, ServiceRequest};

use super::graph_build::{build_core_graph, GmemFlowConfig};
use super::request::{extract_gmem_request, GmemCompletion, GmemIssue, GmemReject, GmemRejectReason, GmemRequest};
use super::stats::GmemStats;

pub(crate) struct GmemSubgraph {
    ingress_node: crate::timeflow::types::NodeId,
    return_node: crate::timeflow::types::NodeId,
    pub(crate) completions: VecDeque<GmemCompletion>,
    next_id: u64,
    pub(crate) stats: GmemStats,
}

impl GmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &GmemFlowConfig) -> Self {
        let nodes = build_core_graph(graph, config);
        Self {
            ingress_node: nodes.ingress_node,
            return_node: nodes.return_node,
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
            self.next_id
        } else {
            request.id
        };
        request.id = assigned_id;

        let bytes = request.bytes;
        let payload = CoreFlowPayload::Gmem(request);
        let service_req = ServiceRequest::new(payload, bytes);

        match graph.try_put(self.ingress_node, now, service_req) {
            Ok(ticket) => {
                if assigned_id >= self.next_id {
                    self.next_id = assigned_id.saturating_add(1);
                }
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
        graph.with_node_mut(self.return_node, |node| {
            while let Some(result) = node.take_ready(now) {
                match result.payload {
                    CoreFlowPayload::Gmem(request) => {
                        self.stats.completed = self.stats.completed.saturating_add(1);
                        self.stats.bytes_completed =
                            self.stats.bytes_completed.saturating_add(request.bytes as u64);
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
