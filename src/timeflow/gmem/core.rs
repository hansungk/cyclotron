use std::collections::VecDeque;

use crate::timeflow::graph::FlowGraph;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{normalize_retry, Backpressure, Cycle, ServiceRequest};

use super::graph_build::{build_core_graph, GmemFlowConfig};
use super::request::{
    extract_gmem_request, GmemCompletion, GmemIssue, GmemReject, GmemRejectReason, GmemRequest,
    GmemResult,
};
use super::stats::GmemStats;

pub(crate) struct GmemSubgraph {
    ingress_node: NodeId,
    return_node: NodeId,
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
    ) -> GmemResult<GmemIssue> {
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
                self.stats.record_issue(bytes);
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
                    self.stats.record_busy_reject();
                    let retry_at = normalize_retry(now, available_at);
                    let request = extract_gmem_request(request);
                    Err(GmemReject {
                        payload: request,
                        retry_at,
                        reason: GmemRejectReason::Busy,
                    })
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.record_queue_full_reject();
                    let retry_at = now.saturating_add(1);
                    let request = extract_gmem_request(request);
                    Err(GmemReject {
                        payload: request,
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
                if let CoreFlowPayload::Gmem(request) = result.payload {
                    self.stats.record_completion(request.bytes, now);
                    completions.push_back(GmemCompletion {
                        ticket_ready_at: result.ticket.ready_at(),
                        completed_at: now,
                        request,
                    });
                }
            }
        });
        self.stats.update_completion_queue(self.completions.len());
    }
}
