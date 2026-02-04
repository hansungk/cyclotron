use serde::{Deserialize, Serialize};

use crate::timeflow::{
    gmem::GmemRequest,
    graph::{FlowGraph, Link},
    server_node::ServerNode,
    smem::SmemRequest,
    types::NodeId,
};
use crate::timeq::{
    normalize_retry, Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer,
};

#[derive(Debug, Clone)]
pub enum LsuPayload {
    Gmem(GmemRequest),
    Smem(SmemRequest),
}

impl LsuPayload {
    pub(crate) fn bytes(&self) -> u32 {
        match self {
            Self::Gmem(req) => req.bytes.max(1),
            Self::Smem(req) => req.bytes.max(1),
        }
    }

    pub(crate) fn warp(&self) -> usize {
        match self {
            Self::Gmem(req) => req.warp,
            Self::Smem(req) => req.warp,
        }
    }

    pub(crate) fn queue_kind(&self) -> LsuQueueKind {
        match self {
            Self::Gmem(req) => {
                if req.kind.is_mem() {
                    if req.is_load {
                        LsuQueueKind::GlobalLoad
                    } else {
                        LsuQueueKind::GlobalStore
                    }
                } else {
                    LsuQueueKind::GlobalStore
                }
            }
            Self::Smem(req) => {
                if req.is_store {
                    LsuQueueKind::SharedStore
                } else {
                    LsuQueueKind::SharedLoad
                }
            }
        }
    }

    pub(crate) fn needs_address(&self) -> bool {
        match self {
            LsuPayload::Gmem(req) => {
                req.kind.is_mem() || req.kind.is_flush_l0() || req.kind.is_flush_l1()
            }
            LsuPayload::Smem(_) => true,
        }
    }

    pub(crate) fn needs_store_data(&self) -> bool {
        match self {
            LsuPayload::Gmem(req) => req.kind.is_mem() && !req.is_load,
            LsuPayload::Smem(req) => req.is_store,
        }
    }

    pub(crate) fn needs_load_data(&self) -> bool {
        match self {
            LsuPayload::Gmem(req) => req.kind.is_mem() && req.is_load,
            LsuPayload::Smem(req) => !req.is_store,
        }
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LsuQueueKind {
    GlobalLoad,
    GlobalStore,
    SharedLoad,
    SharedStore,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LsuStats {
    pub issued: u64,
    pub completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
}

#[derive(Debug, Clone)]
pub struct LsuCompletion<T> {
    pub request: T,
    pub ticket_ready_at: Cycle,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone)]
pub struct LsuIssue {
    pub ticket: Ticket,
}

pub use crate::timeflow::types::RejectReason as LsuRejectReason;

pub type LsuReject = crate::timeflow::types::RejectWith<LsuPayload>;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LsuQueueConfig {
    pub global_ldq: ServerConfig,
    pub global_stq: ServerConfig,
    pub shared_ldq: ServerConfig,
    pub shared_stq: ServerConfig,
}

impl Default for LsuQueueConfig {
    fn default() -> Self {
        let queue = |entries| ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 1024,
            queue_capacity: entries,
            completions_per_cycle: 1,
            ..ServerConfig::default()
        };
        Self {
            global_ldq: queue(8),
            global_stq: queue(4),
            shared_ldq: queue(4),
            shared_stq: queue(2),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LsuResourceConfig {
    pub address_entries: usize,
    pub store_data_entries: usize,
    pub load_data_entries: usize,
}

impl Default for LsuResourceConfig {
    fn default() -> Self {
        Self {
            address_entries: 16,
            store_data_entries: 8,
            load_data_entries: 16,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LsuFlowConfig {
    pub queues: LsuQueueConfig,
    pub resources: LsuResourceConfig,
    pub issue: ServerConfig,
    pub link_capacity: usize,
}

impl Default for LsuFlowConfig {
    fn default() -> Self {
        Self {
            queues: LsuQueueConfig::default(),
            resources: LsuResourceConfig::default(),
            issue: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 1024,
                queue_capacity: 1,
                completions_per_cycle: 1,
                ..ServerConfig::default()
            },
            link_capacity: 4,
        }
    }
}

struct WarpQueues {
    global_ldq: NodeId,
    global_stq: NodeId,
    shared_ldq: NodeId,
    shared_stq: NodeId,
}

pub struct LsuSubgraph {
    graph: FlowGraph<LsuPayload>,
    issue_node: NodeId,
    queues: Vec<WarpQueues>,
    store_pending_global: Vec<u32>,
    store_pending_shared: Vec<u32>,
    resources: LsuResourceConfig,
    address_in_use: usize,
    store_in_use: usize,
    load_in_use: usize,
    stats: LsuStats,
}

impl LsuSubgraph {
    pub fn new(config: LsuFlowConfig, num_warps: usize) -> Self {
        let mut graph = FlowGraph::new();
        let issue_node =
            graph.add_node(ServerNode::new("lsu_issue", TimedServer::new(config.issue)));

        let mut queues = Vec::with_capacity(num_warps);
        for warp in 0..num_warps {
            let global_ldq = graph.add_node(ServerNode::new(
                format!("lsu_global_ldq_w{warp}"),
                TimedServer::new(config.queues.global_ldq),
            ));
            let global_stq = graph.add_node(ServerNode::new(
                format!("lsu_global_stq_w{warp}"),
                TimedServer::new(config.queues.global_stq),
            ));
            let shared_ldq = graph.add_node(ServerNode::new(
                format!("lsu_shared_ldq_w{warp}"),
                TimedServer::new(config.queues.shared_ldq),
            ));
            let shared_stq = graph.add_node(ServerNode::new(
                format!("lsu_shared_stq_w{warp}"),
                TimedServer::new(config.queues.shared_stq),
            ));
            queues.push(WarpQueues {
                global_ldq,
                global_stq,
                shared_ldq,
                shared_stq,
            });
        }

        // Match RTL memReqArbiter priority:
        // shared loads, shared stores, global loads, global stores (warp 0..N each).
        for (warp, nodes) in queues.iter().enumerate() {
            graph.connect(
                nodes.shared_ldq,
                issue_node,
                format!("lsu_shared_ldq_w{warp}->issue"),
                Link::new(config.link_capacity),
            );
        }
        for (warp, nodes) in queues.iter().enumerate() {
            graph.connect(
                nodes.shared_stq,
                issue_node,
                format!("lsu_shared_stq_w{warp}->issue"),
                Link::new(config.link_capacity),
            );
        }
        for (warp, nodes) in queues.iter().enumerate() {
            graph.connect(
                nodes.global_ldq,
                issue_node,
                format!("lsu_global_ldq_w{warp}->issue"),
                Link::new(config.link_capacity),
            );
        }
        for (warp, nodes) in queues.iter().enumerate() {
            graph.connect(
                nodes.global_stq,
                issue_node,
                format!("lsu_global_stq_w{warp}->issue"),
                Link::new(config.link_capacity),
            );
        }

        let num_warps = queues.len();
        Self {
            graph,
            issue_node,
            queues,
            store_pending_global: vec![0; num_warps],
            store_pending_shared: vec![0; num_warps],
            resources: config.resources,
            address_in_use: 0,
            store_in_use: 0,
            load_in_use: 0,
            stats: LsuStats::default(),
        }
    }

    pub fn issue_gmem(&mut self, now: Cycle, request: GmemRequest) -> Result<LsuIssue, LsuReject> {
        self.issue_payload(now, LsuPayload::Gmem(request))
    }

    pub fn issue_smem(&mut self, now: Cycle, request: SmemRequest) -> Result<LsuIssue, LsuReject> {
        self.issue_payload(now, LsuPayload::Smem(request))
    }

    pub fn issue_payload(
        &mut self,
        now: Cycle,
        payload: LsuPayload,
    ) -> Result<LsuIssue, LsuReject> {
        let retry_next = now.saturating_add(1);
        if self.load_blocked_by_store(&payload) {
            return Err(LsuReject::new(payload, retry_next, LsuRejectReason::Busy));
        }

        if !self.can_reserve(&payload) {
            return Err(LsuReject::new(
                payload,
                retry_next,
                LsuRejectReason::QueueFull,
            ));
        }

        let warp = payload.warp();
        let kind = payload.queue_kind();
        let node_id = match self.queue_node(warp, kind) {
            Some(node) => node,
            None => {
                return Err(LsuReject::new(
                    payload,
                    retry_next,
                    LsuRejectReason::QueueFull,
                ))
            }
        };
        let payload_clone = payload.clone();
        let size_bytes = payload.bytes();
        let service_req = ServiceRequest::new(payload, size_bytes);
        match self.graph.try_put(node_id, now, service_req) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                self.bump_store_pending(&payload_clone, true);
                if Self::needs_address(&payload_clone) {
                    self.address_in_use = self.address_in_use.saturating_add(1);
                }
                if Self::needs_store_data(&payload_clone) {
                    self.store_in_use = self.store_in_use.saturating_add(1);
                }
                Ok(LsuIssue { ticket })
            }
            Err(bp) => match bp {
                Backpressure::Busy {
                    request,
                    available_at,
                } => {
                    self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                    let retry_at = normalize_retry(now, available_at);
                    Err(LsuReject::new(
                        request.payload,
                        retry_at,
                        LsuRejectReason::Busy,
                    ))
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.queue_full_rejects = self.stats.queue_full_rejects.saturating_add(1);
                    Err(LsuReject::new(
                        request.payload,
                        retry_next,
                        LsuRejectReason::QueueFull,
                    ))
                }
            },
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.graph.tick(now);
    }

    pub fn stats(&self) -> LsuStats {
        self.stats
    }

    pub fn clear_stats(&mut self) {
        self.stats = LsuStats::default();
        // Reset resource usage counters to zero. These track ephemeral
        // reservation counts and should be cleared when resetting stats.
        self.address_in_use = 0;
        self.store_in_use = 0;
        self.load_in_use = 0;
    }

    pub fn release_issue_resources(&mut self, payload: &LsuPayload) {
        if payload.needs_address() {
            self.address_in_use = self.address_in_use.saturating_sub(1);
        }
        if payload.needs_store_data() {
            self.store_in_use = self.store_in_use.saturating_sub(1);
        }
    }

    pub fn reserve_load_data(&mut self, payload: &LsuPayload) -> bool {
        if !payload.needs_load_data() {
            return true;
        }
        if self.load_in_use >= self.resources.load_data_entries {
            return false;
        }
        self.load_in_use = self.load_in_use.saturating_add(1);
        true
    }

    pub fn release_load_data(&mut self, payload: &LsuPayload) {
        if payload.needs_load_data() {
            self.load_in_use = self.load_in_use.saturating_sub(1);
        }
    }

    pub fn peek_ready(&mut self, now: Cycle) -> Option<LsuPayload> {
        self.graph.with_node_mut(self.issue_node, |node| {
            node.peek_ready(now)
                .map(|result| result.payload.clone())
        })
    }

    pub fn take_ready(&mut self, now: Cycle) -> Option<LsuCompletion<LsuPayload>> {
        let result = self
            .graph
            .with_node_mut(self.issue_node, |node| node.take_ready(now));
        result.map(|result| {
            self.stats.completed = self.stats.completed.saturating_add(1);
            self.bump_store_pending(&result.payload, false);
            LsuCompletion {
                request: result.payload,
                ticket_ready_at: result.ticket.ready_at(),
                completed_at: now,
            }
        })
    }

    fn queue_node(&self, warp: usize, kind: LsuQueueKind) -> Option<NodeId> {
        let queues = self.queues.get(warp)?;
        Some(match kind {
            LsuQueueKind::GlobalLoad => queues.global_ldq,
            LsuQueueKind::GlobalStore => queues.global_stq,
            LsuQueueKind::SharedLoad => queues.shared_ldq,
            LsuQueueKind::SharedStore => queues.shared_stq,
        })
    }

    fn load_blocked_by_store(&self, payload: &LsuPayload) -> bool {
        let warp = payload.warp();
        match payload.queue_kind() {
            LsuQueueKind::GlobalLoad => self
                .store_pending_global
                .get(warp)
                .map_or(false, |&pending| pending > 0),
            LsuQueueKind::SharedLoad => self
                .store_pending_shared
                .get(warp)
                .map_or(false, |&pending| pending > 0),
            _ => false,
        }
    }

    fn bump_store_pending(&mut self, payload: &LsuPayload, increment: bool) {
        let warp = payload.warp();
        match payload.queue_kind() {
            LsuQueueKind::GlobalStore => {
                if let Some(slot) = self.store_pending_global.get_mut(warp) {
                    if increment {
                        *slot = slot.saturating_add(1);
                    } else {
                        *slot = slot.saturating_sub(1);
                    }
                }
            }
            LsuQueueKind::SharedStore => {
                if let Some(slot) = self.store_pending_shared.get_mut(warp) {
                    if increment {
                        *slot = slot.saturating_add(1);
                    } else {
                        *slot = slot.saturating_sub(1);
                    }
                }
            }
            _ => {}
        }
    }

    fn needs_address(payload: &LsuPayload) -> bool {
        payload.needs_address()
    }

    fn needs_store_data(payload: &LsuPayload) -> bool {
        payload.needs_store_data()
    }

    fn needs_load_data(payload: &LsuPayload) -> bool {
        payload.needs_load_data()
    }

    pub fn can_reserve_load_data(&self, payload: &LsuPayload) -> bool {
        if !Self::needs_load_data(payload) {
            return true;
        }
        self.load_in_use < self.resources.load_data_entries
    }

    fn can_reserve(&self, payload: &LsuPayload) -> bool {
        if Self::needs_address(payload) && self.address_in_use >= self.resources.address_entries {
            return false;
        }
        if Self::needs_store_data(payload) && self.store_in_use >= self.resources.store_data_entries {
            return false;
        }
        true
    }

}

