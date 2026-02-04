use std::collections::VecDeque;
use std::sync::Arc;

use crate::timeflow::types::{LinkId, NodeId};
use crate::timeq::{Backpressure, Cycle, ServiceRequest, ServiceResult, Ticket, normalize_retry};
use crate::sim::perf_log;

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

                    if let Some(logger) = perf_log::graph_logger() {
                        let (reason, available_at, capacity) = match &bp {
                            Backpressure::Busy { available_at, .. } => {
                                ("busy".to_string(), Some(*available_at), None)
                            }
                            Backpressure::QueueFull { capacity, .. } => {
                                ("queue_full".to_string(), None, Some(*capacity))
                            }
                        };
                        let record = perf_log::GraphBackpressureRecord {
                            cycle: now,
                            edge: self.edges[edge_id]._name.clone(),
                            src: self.nodes[self.edges[edge_id].src].name.clone(),
                            dst: self.nodes[self.edges[edge_id].dst].name.clone(),
                            reason,
                            retry_at,
                            available_at,
                            capacity,
                            size_bytes: ticket.size_bytes(),
                        };
                        logger.write_json(&record);
                    }

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

impl<T: Send + Sync + 'static> Default for FlowGraph<T> {
    fn default() -> Self {
        Self::new()
    }
}
