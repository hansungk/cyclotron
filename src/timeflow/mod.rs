use std::collections::VecDeque;
use std::sync::Arc;

use crate::timeq::{Backpressure, Cycle, ServiceRequest, ServiceResult, Ticket, TimedServer};

pub type NodeId = usize;
pub type LinkId = usize;

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
        predicate: Option<EdgePredicate<T>>,
    ) -> Self {
        Self {
            _name: name.into(),
            buffer,
            src,
            dst,
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

pub struct ServerNode<T> {
    name: String,
    server: TimedServer<T>,
}

impl<T> ServerNode<T> {
    pub fn new(name: impl Into<String>, server: TimedServer<T>) -> Self {
        Self {
            name: name.into(),
            server,
        }
    }
}

impl<T: Send + Sync> TimedNode<T> for ServerNode<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn try_put(
        &mut self,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>> {
        self.server.try_enqueue(now, request)
    }

    fn tick(&mut self, now: Cycle) {
        self.server.advance_ready(now);
    }

    fn peek_ready(&mut self, now: Cycle) -> Option<&ServiceResult<T>> {
        self.server.peek_ready(now)
    }

    fn take_ready(&mut self, now: Cycle) -> Option<ServiceResult<T>> {
        self.server.pop_ready(now)
    }

    fn outstanding(&self) -> usize {
        self.server.outstanding()
    }
}

struct GraphNode<T> {
    name: String,
    node: Box<dyn TimedNode<T>>,
    outputs: Vec<LinkId>,
    inputs: Vec<LinkId>,
}

impl<T> GraphNode<T> {
    fn new(name: String, node: Box<dyn TimedNode<T>>) -> Self {
        Self {
            name,
            node,
            outputs: Vec::new(),
            inputs: Vec::new(),
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
        self.edges
            .push(Edge::new(link_name, buffer, src, dst, predicate));
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
                            let predicate_pass = self.edges[edge_id]
                                .predicate
                                .as_ref()
                                .map(|pred| pred(&result.payload))
                                .unwrap_or(true);
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
                        let min_retry = now.saturating_add(1);
                        let retry_at = match &bp {
                            Backpressure::Busy { available_at, .. } => *available_at,
                            Backpressure::QueueFull { .. } => min_retry,
                        };

                        let request = bp.into_request();
                        let restored = LinkEntry::from_parts(request, ticket);
                        self.edges[edge_id].buffer.push_front(restored);
                        self.edges[edge_id].stats.downstream_backpressure += 1;
                        self.edges[edge_id].next_retry_cycle =
                            if retry_at <= now { min_retry } else { retry_at };
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
    pub coalescer: crate::timeq::ServerConfig,
    pub cache: crate::timeq::ServerConfig,
    pub dram: crate::timeq::ServerConfig,
    pub link_capacity: usize,
}

impl Default for GmemFlowConfig {
    fn default() -> Self {
        use crate::timeq::ServerConfig;
        Self {
            coalescer: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 16,
                queue_capacity: 16,
                completions_per_cycle: u32::MAX,
                warmup_latency: 0,
            },
            cache: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 32,
                completions_per_cycle: u32::MAX,
                warmup_latency: 0,
            },
            dram: ServerConfig {
                base_latency: 80,
                bytes_per_cycle: 16,
                queue_capacity: 64,
                completions_per_cycle: u32::MAX,
                warmup_latency: 0,
            },
            link_capacity: 16,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CoreFlowPayload {
    Gmem(GmemRequest),
    Smem(SmemRequest),
}

#[derive(Debug, Clone)]
pub struct CoreGraphConfig {
    pub gmem: GmemFlowConfig,
    pub smem: SmemFlowConfig,
}

impl Default for CoreGraphConfig {
    fn default() -> Self {
        Self {
            gmem: GmemFlowConfig::default(),
            smem: SmemFlowConfig::default(),
        }
    }
}

struct GmemSubgraph {
    ingress_node: NodeId,
    dram_node: NodeId,
    completions: VecDeque<GmemCompletion>,
    next_id: u64,
    stats: GmemStats,
}

impl GmemSubgraph {
    fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &GmemFlowConfig) -> Self {
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

    fn issue(
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

    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
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

fn extract_gmem_request(request: ServiceRequest<CoreFlowPayload>) -> GmemRequest {
    match request.payload {
        CoreFlowPayload::Gmem(req) => req,
        _ => panic!("expected gmem request"),
    }
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
pub struct SmemFlowConfig {
    pub crossbar: crate::timeq::ServerConfig,
    pub bank: crate::timeq::ServerConfig,
    pub num_banks: usize,
    pub link_capacity: usize,
}

impl Default for SmemFlowConfig {
    fn default() -> Self {
        use crate::timeq::ServerConfig;
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

struct SmemSubgraph {
    ingress_node: NodeId,
    bank_nodes: Vec<NodeId>,
    completions: VecDeque<SmemCompletion>,
    next_id: u64,
    stats: SmemStats,
}

impl SmemSubgraph {
    fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &SmemFlowConfig) -> Self {
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

    fn issue(
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

    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
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

fn extract_smem_request(request: ServiceRequest<CoreFlowPayload>) -> SmemRequest {
    match request.payload {
        CoreFlowPayload::Smem(req) => req,
        _ => panic!("expected smem request"),
    }
}

pub struct CoreGraph {
    graph: FlowGraph<CoreFlowPayload>,
    gmem: GmemSubgraph,
    smem: SmemSubgraph,
}

impl CoreGraph {
    pub fn new(config: CoreGraphConfig) -> Self {
        let mut graph = FlowGraph::new();
        let gmem = GmemSubgraph::attach(&mut graph, &config.gmem);
        let smem = SmemSubgraph::attach(&mut graph, &config.smem);
        Self { graph, gmem, smem }
    }

    pub fn issue_gmem(
        &mut self,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        self.gmem.issue(&mut self.graph, now, request)
    }

    pub fn tick(&mut self, now: Cycle) {
        self.graph.tick(now);
        self.gmem.collect_completions(&mut self.graph, now);
        self.smem.collect_completions(&mut self.graph, now);
    }

    pub fn pop_gmem_completion(&mut self) -> Option<GmemCompletion> {
        self.gmem.completions.pop_front()
    }

    pub fn pending_gmem_completions(&self) -> usize {
        self.gmem.completions.len()
    }

    pub fn gmem_stats(&self) -> GmemStats {
        self.gmem.stats
    }

    pub fn clear_gmem_stats(&mut self) {
        self.gmem.stats = GmemStats::default();
    }

    pub fn issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        self.smem.issue(&mut self.graph, now, request)
    }

    pub fn pop_smem_completion(&mut self) -> Option<SmemCompletion> {
        self.smem.completions.pop_front()
    }

    pub fn pending_smem_completions(&self) -> usize {
        self.smem.completions.len()
    }

    pub fn smem_stats(&self) -> SmemStats {
        self.smem.stats
    }

    pub fn clear_smem_stats(&mut self) {
        self.smem.stats = SmemStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeq::ServerConfig;

    #[test]
    fn flow_graph_pipelines_work_through_nodes() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();

        let node0 = ServerNode::new(
            "coalescer",
            TimedServer::new(ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        );
        let node1 = ServerNode::new(
            "dram",
            TimedServer::new(ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        );

        let n0 = graph.add_node(node0);
        let n1 = graph.add_node(node1);

        graph.connect(n0, n1, "coalescer->dram", Link::new(4));

        graph
            .try_put(n0, 0, ServiceRequest::new("req0", 8))
            .expect("initial enqueue should succeed");

        let mut drain_log = Vec::new();
        for cycle in 0..10 {
            graph.tick(cycle);
            graph.with_node_mut(n1, |node| {
                while let Some(result) = node.take_ready(cycle) {
                    drain_log.push((cycle, result.ticket.ready_at(), result.payload));
                }
            });
        }

        assert_eq!(1, drain_log.len());
        let (cycle, ready_at, payload) = drain_log[0];
        assert_eq!("req0", payload);
        assert!(cycle >= ready_at);
        assert_eq!(7, ready_at);
    }

    #[test]
    fn downstream_backpressure_requeues_on_link() {
        let mut graph: FlowGraph<&'static str> = FlowGraph::new();

        let node0 = ServerNode::new(
            "upstream",
            TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 4,
                queue_capacity: 4,
                ..ServerConfig::default()
            }),
        );
        let node1 = ServerNode::new(
            "downstream",
            TimedServer::new(ServerConfig {
                base_latency: 10,
                bytes_per_cycle: 4,
                queue_capacity: 1,
                ..ServerConfig::default()
            }),
        );

        let n0 = graph.add_node(node0);
        let n1 = graph.add_node(node1);
        graph.connect(n0, n1, "link", Link::new(1));

        graph
            .try_put(n0, 0, ServiceRequest::new("req0", 4))
            .expect("enqueue should succeed");
        graph
            .try_put(n0, 1, ServiceRequest::new("req1", 4))
            .expect("second enqueue should succeed due to queueing");

        for cycle in 0..20 {
            graph.tick(cycle);
            graph.with_node_mut(n1, |node| {
                // Consume at most one completion per cycle to force backpressure.
                if let Some(result) = node.take_ready(cycle) {
                    let _ = result;
                }
            });
        }

        let stats = graph.edge_stats(0);
        assert!(stats.downstream_backpressure > 0);
        assert_eq!(2, stats.entries_pushed);
        assert_eq!(2, stats.entries_delivered);
    }

    #[test]
    fn core_graph_gmem_flow_completes_requests() {
        let mut cfg = CoreGraphConfig::default();
        cfg.gmem.coalescer = ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        };
        cfg.gmem.cache = ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        };
        cfg.gmem.dram = ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        };

        let mut core_graph = CoreGraph::new(cfg);

        let request = GmemRequest::new(0, 16, 0xF, true);
        let issue = core_graph
            .issue_gmem(0, request)
            .expect("GMEM request should be accepted");

        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(100) {
            core_graph.tick(cycle);
        }

        assert!(
            core_graph.pending_gmem_completions() > 0,
            "expected at least one completion by cycle {}, but none were ready",
            ready_at
        );
        let completion = core_graph
            .pop_gmem_completion()
            .expect("completion expected after ticking");
        assert_eq!(0, completion.request.warp);
        assert!(completion.completed_at >= ready_at);
    }
}
