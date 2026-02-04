use std::collections::VecDeque;
use std::sync::Arc;

use crate::timeflow::types::{LinkId, NodeId};
use crate::timeq::{Backpressure, Cycle, ServiceRequest, ServiceResult, Ticket, normalize_retry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkBackpressure {
    Capacity { capacity: usize },
    Bytes { capacity: u32 },
}

#[derive(Debug)]
struct LinkEntry<T> {
    result: ServiceResult<T>,
    size_bytes: u32,
}

impl<T> LinkEntry<T> {
    fn new(result: ServiceResult<T>) -> Self {
        let size_bytes = result.ticket.size_bytes();
        Self { result, size_bytes }
    }

    fn size_bytes(&self) -> u32 {
        self.size_bytes
    }

    fn into_request(self) -> (ServiceRequest<T>, Ticket) {
        let LinkEntry { result, size_bytes } = self;
        let ServiceResult { payload, ticket } = result;
        (ServiceRequest::new(payload, size_bytes), ticket)
    }

    fn from_parts(request: ServiceRequest<T>, ticket: Ticket) -> Self {
        let ServiceRequest {
            payload,
            size_bytes,
        } = request;
        let result = ServiceResult { payload, ticket };
        Self { result, size_bytes }
    }
}

#[derive(Debug)]
pub struct Link<T> {
    entries_capacity: usize,
    bytes_capacity: Option<u32>,
    bytes_in_use: u32,
    queue: VecDeque<LinkEntry<T>>,
}

impl<T> Link<T> {
    pub fn new(entries_capacity: usize) -> Self {
        Self::with_byte_limit(entries_capacity, None)
    }

    pub fn with_byte_limit(entries_capacity: usize, bytes_capacity: Option<u32>) -> Self {
        assert!(entries_capacity > 0, "link capacity must be > 0");
        Self {
            entries_capacity,
            bytes_capacity,
            bytes_in_use: 0,
            queue: VecDeque::with_capacity(entries_capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn can_accept(&self, size_bytes: u32) -> bool {
        if self.queue.len() >= self.entries_capacity {
            return false;
        }
        if let Some(limit) = self.bytes_capacity {
            if self.bytes_in_use + size_bytes > limit {
                return false;
            }
        }
        true
    }

    fn try_push(&mut self, result: ServiceResult<T>) -> Result<(), LinkBackpressure> {
        let size_bytes = result.ticket.size_bytes();
        if self.queue.len() >= self.entries_capacity {
            return Err(LinkBackpressure::Capacity {
                capacity: self.entries_capacity,
            });
        }
        if let Some(limit) = self.bytes_capacity {
            if self.bytes_in_use + size_bytes > limit {
                return Err(LinkBackpressure::Bytes { capacity: limit });
            }
        }

        self.queue.push_back(LinkEntry::new(result));
        self.bytes_in_use = self.bytes_in_use.saturating_add(size_bytes);
        Ok(())
    }

    fn pop_front(&mut self) -> Option<LinkEntry<T>> {
        let entry = self.queue.pop_front()?;
        self.bytes_in_use = self.bytes_in_use.saturating_sub(entry.size_bytes());
        Some(entry)
    }

    fn push_front(&mut self, entry: LinkEntry<T>) {
        self.bytes_in_use = self.bytes_in_use.saturating_add(entry.size_bytes());
        self.queue.push_front(entry);
    }
}

#[derive(Debug, Default, Clone)]
pub struct EdgeStats {
    pub entries_pushed: u64,
    pub entries_delivered: u64,
    pub downstream_backpressure: u64,
    pub last_delivery_cycle: Option<Cycle>,
}

type EdgePredicate<T> = Arc<dyn Fn(&T) -> bool + Send + Sync>;

struct Edge<T> {
    _name: String,
    buffer: Link<T>,
    src: NodeId,
    dst: NodeId,
    output_idx: usize,
    stats: EdgeStats,
    next_retry_cycle: Cycle,
    predicate: Option<EdgePredicate<T>>,
}

impl<T> Edge<T> {
    fn new(
        name: impl Into<String>,
        buffer: Link<T>,
        src: NodeId,
        dst: NodeId,
        output_idx: usize,
        predicate: Option<EdgePredicate<T>>,
    ) -> Self {
        Self {
            _name: name.into(),
            buffer,
            src,
            dst,
            output_idx,
            stats: EdgeStats::default(),
            next_retry_cycle: 0,
            predicate,
        }
    }
}

pub trait TimedNode<T>: Send + Sync {
    fn name(&self) -> &str;
    fn try_put(
        &mut self,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>>;
    fn tick(&mut self, now: Cycle);
    fn peek_ready(&mut self, now: Cycle) -> Option<&ServiceResult<T>>;
    fn take_ready(&mut self, now: Cycle) -> Option<ServiceResult<T>>;
    fn outstanding(&self) -> usize;
}

struct GraphNode<T> {
    name: String,
    node: Box<dyn TimedNode<T>>,
    outputs: Vec<LinkId>,
    inputs: Vec<LinkId>,
    route_fn: Option<Arc<dyn Fn(&T) -> usize + Send + Sync>>,
}

impl<T> GraphNode<T> {
    fn new(name: String, node: Box<dyn TimedNode<T>>) -> Self {
        Self {
            name,
            node,
            outputs: Vec::new(),
            inputs: Vec::new(),
            route_fn: None,
        }
    }
}

pub struct FlowGraph<T> {
    nodes: Vec<GraphNode<T>>,
    edges: Vec<Edge<T>>,
}

impl<T: Send + Sync + 'static> FlowGraph<T> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node<N>(&mut self, node: N) -> NodeId
    where
        N: TimedNode<T> + 'static,
    {
        let name = node.name().to_string();
        let boxed: Box<dyn TimedNode<T>> = Box::new(node);
        let id = self.nodes.len();
        self.nodes.push(GraphNode::new(name, boxed));
        id
    }

    fn connect_internal(
        &mut self,
        src: NodeId,
        dst: NodeId,
        link_name: impl Into<String>,
        buffer: Link<T>,
        predicate: Option<EdgePredicate<T>>,
    ) -> LinkId {
        assert!(src < self.nodes.len(), "invalid src node");
        assert!(dst < self.nodes.len(), "invalid dst node");
        let id = self.edges.len();
        let output_idx = self.nodes[src].outputs.len();
        self.edges.push(Edge::new(
            link_name, buffer, src, dst, output_idx, predicate,
        ));
        self.nodes[src].outputs.push(id);
        self.nodes[dst].inputs.push(id);
        id
    }

    pub fn connect(
        &mut self,
        src: NodeId,
        dst: NodeId,
        link_name: impl Into<String>,
        buffer: Link<T>,
    ) -> LinkId {
        self.connect_internal(src, dst, link_name, buffer, None)
    }

    pub fn connect_filtered(
        &mut self,
        src: NodeId,
        dst: NodeId,
        link_name: impl Into<String>,
        buffer: Link<T>,
        predicate: impl Fn(&T) -> bool + Send + Sync + 'static,
    ) -> LinkId {
        self.connect_internal(src, dst, link_name, buffer, Some(Arc::new(predicate)))
    }

    pub fn set_route_fn(
        &mut self,
        node_id: NodeId,
        route_fn: impl Fn(&T) -> usize + Send + Sync + 'static,
    ) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.route_fn = Some(Arc::new(route_fn));
        }
    }

    pub fn try_put(
        &mut self,
        node_id: NodeId,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>> {
        self.nodes[node_id].node.try_put(now, request)
    }

    pub fn tick(&mut self, now: Cycle) {
        for node in &mut self.nodes {
            node.node.tick(now);
        }

        for edge_id in 0..self.edges.len() {
            let src = self.edges[edge_id].src;
            loop {
                let (size_bytes, should_route) = {
                    let src_node = &mut self.nodes[src];
                    match src_node.node.peek_ready(now) {
                        Some(result) => {
                            let routed_idx = src_node
                                .route_fn
                                .as_ref()
                                .map(|route| route(&result.payload));
                            let predicate_pass = if let Some(route_idx) = routed_idx {
                                route_idx == self.edges[edge_id].output_idx
                            } else {
                                self.edges[edge_id]
                                    .predicate
                                    .as_ref()
                                    .map(|pred| pred(&result.payload))
                                    .unwrap_or(true)
                            };
                            (result.ticket.size_bytes(), predicate_pass)
                        }
                        None => break,
                    }
                };

                if !should_route {
                    break;
                }

                if !self.edges[edge_id].buffer.can_accept(size_bytes) {
                    break;
                }

                let result = self.nodes[src]
                    .node
                    .take_ready(now)
                    .expect("peek_ready indicated availability");
                self.edges[edge_id]
                    .buffer
                    .try_push(result)
                    .expect("capacity checked prior to push");
                self.edges[edge_id].stats.entries_pushed += 1;
            }
        }

        for edge_id in 0..self.edges.len() {
            if now < self.edges[edge_id].next_retry_cycle {
                continue;
            }

            let dst = self.edges[edge_id].dst;
            loop {
                let entry = match self.edges[edge_id].buffer.pop_front() {
                    Some(entry) => entry,
                    None => {
                        self.edges[edge_id].next_retry_cycle = now;
                        break;
                    }
                };

                let (request, ticket) = entry.into_request();
                match self.nodes[dst].node.try_put(now, request) {
                    Ok(_) => {
                        self.edges[edge_id].stats.entries_delivered += 1;
                        self.edges[edge_id].stats.last_delivery_cycle = Some(now);
                        self.edges[edge_id].next_retry_cycle = now;
                    }
                Err(bp) => {
                    let retry_at = match &bp {
                        Backpressure::Busy { available_at, .. } => {
                            normalize_retry(now, *available_at)
                        }
                        Backpressure::QueueFull { .. } => normalize_retry(now, now),
                    };

                    let request = bp.into_request();
                    let restored = LinkEntry::from_parts(request, ticket);
                    self.edges[edge_id].buffer.push_front(restored);
                    self.edges[edge_id].stats.downstream_backpressure += 1;
                    self.edges[edge_id].next_retry_cycle = retry_at;
                    break;
                }
                }
            }
        }
    }

    pub fn with_node_mut<R>(
        &mut self,
        node_id: NodeId,
        f: impl FnOnce(&mut dyn TimedNode<T>) -> R,
    ) -> R {
        f(self.nodes[node_id].node.as_mut())
    }

    pub fn node_name(&self, node_id: NodeId) -> &str {
        &self.nodes[node_id].name
    }

    pub fn edge_stats(&self, link_id: LinkId) -> &EdgeStats {
        &self.edges[link_id].stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeflow::server_node::ServerNode;
    use crate::timeq::{ServerConfig, ServiceRequest, TimedServer};

    #[test]
    fn graph_moves_payloads_between_nodes() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let node0 = ServerNode::new(
            "n0",
            TimedServer::new(ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        );
        let node1 = ServerNode::new(
            "n1",
            TimedServer::new(ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        );
        let n0 = graph.add_node(node0);
        let n1 = graph.add_node(node1);
        graph.connect(n0, n1, "n0->n1", Link::new(4));

        graph
            .try_put(n0, 0, ServiceRequest::new("payload", 8))
            .expect("enqueue should succeed");

        for cycle in 0..10 {
            graph.tick(cycle);
        }

        graph.with_node_mut(n1, |node| {
            let result = node.take_ready(10).expect("result should exist");
            assert_eq!("payload", result.payload);
        });
    }

    #[test]
    fn filtered_links_route_payloads() {
        #[derive(Clone, Debug)]
        struct Payload {
            bank: usize,
            value: &'static str,
        }
        let mut graph: FlowGraph<Payload> = FlowGraph::new();
        let src = graph.add_node(ServerNode::new(
            "src",
            TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 4,
                queue_capacity: 8,
                ..ServerConfig::default()
            }),
        ));
        let sink0 = graph.add_node(ServerNode::new(
            "sink0",
            TimedServer::new(ServerConfig::default()),
        ));
        let sink1 = graph.add_node(ServerNode::new(
            "sink1",
            TimedServer::new(ServerConfig::default()),
        ));

        graph.connect_filtered(src, sink0, "src->0", Link::new(4), |payload: &Payload| {
            payload.bank == 0
        });
        graph.connect_filtered(src, sink1, "src->1", Link::new(4), |payload: &Payload| {
            payload.bank == 1
        });

        graph
            .try_put(
                src,
                0,
                ServiceRequest::new(
                    Payload {
                        bank: 0,
                        value: "A",
                    },
                    4,
                ),
            )
            .unwrap();
        graph
            .try_put(
                src,
                1,
                ServiceRequest::new(
                    Payload {
                        bank: 1,
                        value: "B",
                    },
                    4,
                ),
            )
            .unwrap();

        for cycle in 0..10 {
            graph.tick(cycle);
        }

        graph.with_node_mut(sink0, |node| {
            let result = node.take_ready(10).expect("sink0 result");
            assert_eq!("A", result.payload.value);
        });
        graph.with_node_mut(sink1, |node| {
            let result = node.take_ready(10).expect("sink1 result");
            assert_eq!("B", result.payload.value);
        });
    }

    #[test]
    fn route_fn_selects_output_by_index() {
        #[derive(Clone, Debug)]
        struct Payload {
            route: usize,
            value: &'static str,
        }

        let mut graph: FlowGraph<Payload> = FlowGraph::new();
        let src = graph.add_node(ServerNode::new(
            "src",
            TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 4,
                queue_capacity: 8,
                ..ServerConfig::default()
            }),
        ));
        let sink0 = graph.add_node(ServerNode::new(
            "sink0",
            TimedServer::new(ServerConfig::default()),
        ));
        let sink1 = graph.add_node(ServerNode::new(
            "sink1",
            TimedServer::new(ServerConfig::default()),
        ));

        graph.connect(src, sink0, "src->0", Link::new(4));
        graph.connect(src, sink1, "src->1", Link::new(4));
        graph.set_route_fn(src, |payload: &Payload| payload.route);

        graph
            .try_put(
                src,
                0,
                ServiceRequest::new(
                    Payload {
                        route: 0,
                        value: "A",
                    },
                    4,
                ),
            )
            .unwrap();
        graph
            .try_put(
                src,
                1,
                ServiceRequest::new(
                    Payload {
                        route: 1,
                        value: "B",
                    },
                    4,
                ),
            )
            .unwrap();

        for cycle in 0..10 {
            graph.tick(cycle);
        }

        graph.with_node_mut(sink0, |node| {
            let result = node.take_ready(10).expect("sink0 result");
            assert_eq!("A", result.payload.value);
        });
        graph.with_node_mut(sink1, |node| {
            let result = node.take_ready(10).expect("sink1 result");
            assert_eq!("B", result.payload.value);
        });
    }

    #[test]
    fn link_capacity_enforced() {
        let mut link = Link::with_byte_limit(1, Some(8));
        let make_result = |size| {
            let mut server = TimedServer::new(ServerConfig::default());
            let ticket = server
                .try_enqueue(0, ServiceRequest::new("dummy", size))
                .unwrap();
            ServiceResult {
                payload: "req",
                ticket,
            }
        };
        let result = make_result(4);
        assert!(link.try_push(result).is_ok());

        let result = make_result(4);
        assert!(matches!(
            link.try_push(result),
            Err(LinkBackpressure::Capacity { .. })
        ));

        let mut link = Link::with_byte_limit(2, Some(8));
        let result = make_result(6);
        link.try_push(result).unwrap();
        let result = make_result(4);
        assert!(matches!(
            link.try_push(result),
            Err(LinkBackpressure::Bytes { .. })
        ));
    }

    #[test]
    fn single_node_graph_processes_requests() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let node = graph.add_node(ServerNode::new(
            "node",
            TimedServer::new(ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));

        graph
            .try_put(node, 0, ServiceRequest::new("payload", 4))
            .expect("enqueue should succeed");
        for cycle in 0..3 {
            graph.tick(cycle);
        }
        graph.with_node_mut(node, |n| {
            let result = n.take_ready(2).expect("completion exists");
            assert_eq!("payload", result.payload);
        });
    }

    #[test]
    fn two_node_linear_chain_latency_accumulates() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let a = graph.add_node(ServerNode::new(
            "a",
            TimedServer::new(ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));
        let b = graph.add_node(ServerNode::new(
            "b",
            TimedServer::new(ServerConfig {
                base_latency: 3,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));
        graph.connect(a, b, "a->b", Link::new(4));
        graph
            .try_put(a, 0, ServiceRequest::new("payload", 4))
            .expect("enqueue should succeed");

        for cycle in 0..10 {
            graph.tick(cycle);
        }
        graph.with_node_mut(b, |n| {
            let result = n.take_ready(10).expect("completion exists");
            assert_eq!("payload", result.payload);
        });
    }

    #[test]
    fn empty_graph_tick_is_noop() {
        let mut graph: FlowGraph<u32> = FlowGraph::new();
        graph.tick(0);
        graph.tick(1);
    }

    #[test]
    fn node_with_no_outputs_accumulates_completions() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let node = graph.add_node(ServerNode::new(
            "leaf",
            TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 4,
                queue_capacity: 2,
                ..ServerConfig::default()
            }),
        ));
        graph
            .try_put(node, 0, ServiceRequest::new("payload", 4))
            .expect("enqueue should succeed");
        graph.tick(1);
        graph.with_node_mut(node, |n| {
            assert!(n.peek_ready(1).is_some());
        });
    }

    #[test]
    fn filtered_link_with_false_predicate_blocks() {
        let mut graph: FlowGraph<u32> = FlowGraph::new();
        let src = graph.add_node(ServerNode::new(
            "src",
            TimedServer::new(ServerConfig {
                queue_capacity: 32,
                ..ServerConfig::default()
            }),
        ));
        let dst = graph.add_node(ServerNode::new(
            "dst",
            TimedServer::new(ServerConfig::default()),
        ));
        graph.connect_filtered(src, dst, "src->dst", Link::new(2), |_| false);
        graph.try_put(src, 0, ServiceRequest::new(7u32, 4)).unwrap();
        for cycle in 0..5 {
            graph.tick(cycle);
        }
        graph.with_node_mut(dst, |n| {
            assert!(n.peek_ready(10).is_none());
        });
    }

    #[test]
    fn three_node_linear_chain() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let a = graph.add_node(ServerNode::new(
            "a",
            TimedServer::new(ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));
        let b = graph.add_node(ServerNode::new(
            "b",
            TimedServer::new(ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));
        let c = graph.add_node(ServerNode::new(
            "c",
            TimedServer::new(ServerConfig {
                base_latency: 3,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        ));
        graph.connect(a, b, "a->b", Link::new(4));
        graph.connect(b, c, "b->c", Link::new(4));

        graph
            .try_put(a, 0, ServiceRequest::new("payload", 4))
            .expect("enqueue should succeed");
        for cycle in 0..12 {
            graph.tick(cycle);
        }
        graph.with_node_mut(c, |n| {
            let result = n.take_ready(12).expect("completion exists");
            assert_eq!("payload", result.payload);
        });
    }

    #[test]
    fn multiple_inputs_to_one_node() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let src0 = graph.add_node(ServerNode::new(
            "src0",
            TimedServer::new(ServerConfig::default()),
        ));
        let src1 = graph.add_node(ServerNode::new(
            "src1",
            TimedServer::new(ServerConfig::default()),
        ));
        let sink = graph.add_node(ServerNode::new(
            "sink",
            TimedServer::new(ServerConfig {
                queue_capacity: 2,
                ..ServerConfig::default()
            }),
        ));
        graph.connect(src0, sink, "src0->sink", Link::new(4));
        graph.connect(src1, sink, "src1->sink", Link::new(4));

        graph.try_put(src0, 0, ServiceRequest::new("a", 1)).unwrap();
        graph.try_put(src1, 0, ServiceRequest::new("b", 1)).unwrap();
        for cycle in 0..5 {
            graph.tick(cycle);
        }
        graph.with_node_mut(sink, |n| {
            let mut results = Vec::new();
            while let Some(res) = n.take_ready(5) {
                results.push(res.payload);
            }
            results.sort();
            assert_eq!(results, vec!["a", "b"]);
        });
    }

    #[test]
    fn deep_pipeline_100_nodes() {
        let mut graph: FlowGraph<u32> = FlowGraph::new();
        let mut nodes = Vec::new();
        for i in 0..100 {
            let node = graph.add_node(ServerNode::new(
                format!("n{i}"),
                TimedServer::new(ServerConfig {
                    base_latency: 0,
                    bytes_per_cycle: 1,
                    queue_capacity: 2,
                    ..ServerConfig::default()
                }),
            ));
            nodes.push(node);
        }
        for i in 0..99 {
            graph.connect(
                nodes[i],
                nodes[i + 1],
                format!("n{i}->n{}", i + 1),
                Link::new(2),
            );
        }

        graph
            .try_put(nodes[0], 0, ServiceRequest::new(42u32, 1))
            .unwrap();
        for cycle in 0..200 {
            graph.tick(cycle);
        }
        graph.with_node_mut(nodes[99], |n| {
            let result = n.take_ready(200).expect("completion exists");
            assert_eq!(42, result.payload);
        });
    }

    #[test]
    fn wide_fanout_32_branches() {
        #[derive(Clone, Debug)]
        struct Payload {
            branch: usize,
        }
        let mut graph: FlowGraph<Payload> = FlowGraph::new();
        let src = graph.add_node(ServerNode::new(
            "src",
            TimedServer::new(ServerConfig {
                queue_capacity: 32,
                ..ServerConfig::default()
            }),
        ));
        let mut sinks = Vec::new();
        for i in 0..32 {
            let sink = graph.add_node(ServerNode::new(
                format!("sink{i}"),
                TimedServer::new(ServerConfig::default()),
            ));
            graph.connect_filtered(
                src,
                sink,
                format!("src->sink{i}"),
                Link::new(4),
                move |payload: &Payload| payload.branch == i,
            );
            sinks.push(sink);
        }

        for i in 0..32 {
            graph
                .try_put(src, 0, ServiceRequest::new(Payload { branch: i }, 1))
                .unwrap();
        }

        for cycle in 0..50 {
            graph.tick(cycle);
        }

        for (i, &sink) in sinks.iter().enumerate() {
            graph.with_node_mut(sink, |n| {
                let result = n.take_ready(50).expect("completion exists");
                assert_eq!(result.payload.branch, i);
            });
        }
    }

    #[test]
    fn busy_nodes_propagate_backpressure() {
        struct BusyNode;
        impl TimedNode<&'static str> for BusyNode {
            fn name(&self) -> &str {
                "busy"
            }
            fn try_put(
                &mut self,
                now: Cycle,
                request: ServiceRequest<&'static str>,
            ) -> Result<Ticket, Backpressure<&'static str>> {
                Err(Backpressure::Busy {
                    request,
                    available_at: now + 5,
                })
            }
            fn tick(&mut self, _now: Cycle) {}
            fn peek_ready(&mut self, _now: Cycle) -> Option<&ServiceResult<&'static str>> {
                None
            }
            fn take_ready(&mut self, _now: Cycle) -> Option<ServiceResult<&'static str>> {
                None
            }
            fn outstanding(&self) -> usize {
                0
            }
        }

        let mut graph: FlowGraph<&'static str> = FlowGraph::new();
        let src = graph.add_node(ServerNode::new(
            "src",
            TimedServer::new(ServerConfig::default()),
        ));
        let dst = graph.add_node(BusyNode);
        graph.connect(src, dst, "link", Link::new(2));
        graph
            .try_put(src, 0, ServiceRequest::new("req", 4))
            .unwrap();

        for cycle in 0..5 {
            graph.tick(cycle);
        }

        let stats = graph.edge_stats(0);
        assert!(stats.downstream_backpressure > 0);
        assert!(stats.last_delivery_cycle.is_none());
    }
}

impl<T: Send + Sync + 'static> Default for FlowGraph<T> {
    fn default() -> Self {
        Self::new()
    }
}
