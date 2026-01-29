use serde::Deserialize;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::NodeId;
use crate::timeflow::{gmem::GmemRequest, smem::SmemRequest};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone)]
pub enum LsuPayload {
    Gmem(GmemRequest),
    Smem(SmemRequest),
}

impl LsuPayload {
    fn bytes(&self) -> u32 {
        match self {
            Self::Gmem(req) => req.bytes.max(1),
            Self::Smem(req) => req.bytes.max(1),
        }
    }

    fn warp(&self) -> usize {
        match self {
            Self::Gmem(req) => req.warp,
            Self::Smem(req) => req.warp,
        }
    }

    fn queue_kind(&self) -> LsuQueueKind {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LsuQueueKind {
    GlobalLoad,
    GlobalStore,
    SharedLoad,
    SharedStore,
}

#[derive(Debug, Clone, Copy, Default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LsuRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct LsuReject {
    pub request: LsuPayload,
    pub retry_at: Cycle,
    pub reason: LsuRejectReason,
}

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
        let issue_node = graph.add_node(ServerNode::new(
            "lsu_issue",
            TimedServer::new(config.issue),
        ));

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

    pub fn issue_gmem(
        &mut self,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<LsuIssue, LsuReject> {
        self.issue(now, LsuPayload::Gmem(request))
    }

    pub fn issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<LsuIssue, LsuReject> {
        self.issue(now, LsuPayload::Smem(request))
    }

    fn issue(&mut self, now: Cycle, payload: LsuPayload) -> Result<LsuIssue, LsuReject> {
        if self.load_blocked_by_store(&payload) {
            return Err(LsuReject {
                request: payload,
                retry_at: now.saturating_add(1),
                reason: LsuRejectReason::Busy,
            });
        }

        if !self.can_reserve(&payload) {
            return Err(LsuReject {
                request: payload,
                retry_at: now.saturating_add(1),
                reason: LsuRejectReason::QueueFull,
            });
        }

        let warp = payload.warp();
        let kind = payload.queue_kind();
        let node_id = match self.queue_node(warp, kind) {
            Some(node) => node,
            None => {
                return Err(LsuReject {
                    request: payload,
                    retry_at: now.saturating_add(1),
                    reason: LsuRejectReason::QueueFull,
                })
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
                    let retry_at = available_at.max(now.saturating_add(1));
                    Err(LsuReject {
                        request: request.payload,
                        retry_at,
                        reason: LsuRejectReason::Busy,
                    })
                }
                Backpressure::QueueFull { request, .. } => {
                    self.stats.queue_full_rejects =
                        self.stats.queue_full_rejects.saturating_add(1);
                    Err(LsuReject {
                        request: request.payload,
                        retry_at: now.saturating_add(1),
                        reason: LsuRejectReason::QueueFull,
                    })
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

    pub fn release_issue_resources(&mut self, payload: &LsuPayload) {
        if Self::needs_address(payload) && self.address_in_use > 0 {
            self.address_in_use -= 1;
        }
        if Self::needs_store_data(payload) && self.store_in_use > 0 {
            self.store_in_use -= 1;
        }
    }

    pub fn reserve_load_data(&mut self, payload: &LsuPayload) -> bool {
        if !Self::needs_load_data(payload) {
            return true;
        }
        if self.load_in_use >= self.resources.load_data_entries {
            return false;
        }
        self.load_in_use = self.load_in_use.saturating_add(1);
        true
    }

    pub fn release_load_data(&mut self, payload: &LsuPayload) {
        if Self::needs_load_data(payload) && self.load_in_use > 0 {
            self.load_in_use -= 1;
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
        match payload {
            LsuPayload::Gmem(req) => req.kind.is_mem() || req.kind.is_flush_l0() || req.kind.is_flush_l1(),
            LsuPayload::Smem(_) => true,
        }
    }

    fn needs_store_data(payload: &LsuPayload) -> bool {
        match payload {
            LsuPayload::Gmem(req) => req.kind.is_mem() && !req.is_load,
            LsuPayload::Smem(req) => req.is_store,
        }
    }

    fn needs_load_data(payload: &LsuPayload) -> bool {
        match payload {
            LsuPayload::Gmem(req) => req.kind.is_mem() && req.is_load,
            LsuPayload::Smem(req) => !req.is_store,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeflow::gmem::GmemRequestKind;

    fn default_issue_config() -> ServerConfig {
        ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 1024,
            queue_capacity: 1,
            completions_per_cycle: 1,
            ..ServerConfig::default()
        }
    }

    #[test]
    fn lsu_rejects_when_per_warp_queue_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_ldq.queue_capacity = 1;
        config.resources.address_entries = 1;

        let mut lsu = LsuSubgraph::new(config, 1);
        let req0 = GmemRequest::new(0, 16, 0xF, true);
        let req1 = GmemRequest::new(0, 16, 0xF, true);

        assert!(lsu.issue_gmem(0, req0).is_ok());
        let err = lsu.issue_gmem(0, req1).expect_err("queue should be full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn lsu_prioritizes_shared_over_global() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.link_capacity = 1;

        let mut lsu = LsuSubgraph::new(config, 1);
        let gmem_req = GmemRequest::new(0, 16, 0xF, true);
        let smem_req = SmemRequest::new(0, 16, 0xF, false, 0);

        assert!(lsu.issue_gmem(0, gmem_req).is_ok());
        assert!(lsu.issue_smem(0, smem_req).is_ok());

        let mut issued = Vec::new();
        for cycle in 0..10 {
            lsu.tick(cycle);
            while lsu.peek_ready(cycle).is_some() {
                let completion = lsu.take_ready(cycle).expect("ready should exist");
                issued.push(completion.request);
            }
        }

        assert!(
            matches!(issued.get(0), Some(LsuPayload::Smem(_))),
            "expected shared request to issue before global"
        );
        assert!(
            matches!(issued.get(1), Some(LsuPayload::Gmem(_))),
            "expected global request after shared"
        );
    }

    #[test]
    fn lsu_prioritizes_load_over_store_within_space() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.link_capacity = 1;

        let mut lsu = LsuSubgraph::new(config, 1);
        let mut load = GmemRequest::new(0, 16, 0xF, true);
        let mut store = GmemRequest::new(0, 16, 0xF, false);
        load.kind = GmemRequestKind::Load;
        store.kind = GmemRequestKind::Store;

        assert!(lsu.issue_gmem(0, load).is_ok());
        assert!(lsu.issue_gmem(0, store).is_ok());

        let mut issued = Vec::new();
        for cycle in 0..10 {
            lsu.tick(cycle);
            while lsu.peek_ready(cycle).is_some() {
                let completion = lsu.take_ready(cycle).expect("ready should exist");
                issued.push(completion.request);
            }
        }

        let first = issued
            .into_iter()
            .find_map(|payload| match payload {
                LsuPayload::Gmem(req) => Some(req),
                _ => None,
            })
            .expect("expected a gmem request");
        assert!(first.is_load);
    }

    #[test]
    fn per_warp_queues_independent() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_ldq.queue_capacity = 1;
        config.resources.address_entries = 4;

        let mut lsu = LsuSubgraph::new(config, 2);
        let req0 = GmemRequest::new(0, 16, 0xF, true);
        let req1 = GmemRequest::new(0, 16, 0xF, true);
        assert!(lsu.issue_gmem(0, req0).is_ok());
        assert!(lsu.issue_gmem(0, req1).is_err());

        let req_other = GmemRequest::new(1, 16, 0xF, true);
        assert!(lsu.issue_gmem(0, req_other).is_ok());
    }

    #[test]
    fn address_entries_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_ldq.queue_capacity = 2;
        config.resources.address_entries = 1;

        let mut lsu = LsuSubgraph::new(config, 1);
        let req0 = GmemRequest::new(0, 16, 0xF, true);
        let req1 = GmemRequest::new(0, 16, 0xF, true);
        assert!(lsu.issue_gmem(0, req0).is_ok());
        let err = lsu.issue_gmem(0, req1).expect_err("address entries full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn store_data_entries_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_stq.queue_capacity = 2;
        config.resources.store_data_entries = 1;

        let mut lsu = LsuSubgraph::new(config, 1);
        let req0 = GmemRequest::new(0, 16, 0xF, false);
        let req1 = GmemRequest::new(0, 16, 0xF, false);
        assert!(lsu.issue_gmem(0, req0).is_ok());
        let err = lsu.issue_gmem(0, req1).expect_err("store data full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn load_blocked_by_pending_store_same_warp() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_ldq.queue_capacity = 2;
        config.queues.global_stq.queue_capacity = 2;
        config.resources.address_entries = 4;
        config.resources.store_data_entries = 4;

        let mut lsu = LsuSubgraph::new(config, 1);
        let store = GmemRequest::new(0, 16, 0xF, false);
        let load = GmemRequest::new(0, 16, 0xF, true);
        assert!(lsu.issue_gmem(0, store).is_ok());
        let err = lsu.issue_gmem(0, load).expect_err("load should be blocked");
        assert_eq!(err.reason, LsuRejectReason::Busy);
    }

    #[test]
    fn global_stq_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_stq.queue_capacity = 4;
        config.resources.address_entries = 16;
        config.resources.store_data_entries = 4;

        let mut lsu = LsuSubgraph::new(config, 1);
        for _ in 0..4 {
            let req = GmemRequest::new(0, 16, 0xF, false);
            assert!(lsu.issue_gmem(0, req).is_ok());
        }
        let req = GmemRequest::new(0, 16, 0xF, false);
        let err = lsu.issue_gmem(0, req).expect_err("stq should be full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn shared_ldq_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.shared_ldq.queue_capacity = 4;
        config.resources.address_entries = 16;

        let mut lsu = LsuSubgraph::new(config, 1);
        for _ in 0..4 {
            let req = SmemRequest::new(0, 16, 0xF, false, 0);
            assert!(lsu.issue_smem(0, req).is_ok());
        }
        let req = SmemRequest::new(0, 16, 0xF, false, 0);
        let err = lsu.issue_smem(0, req).expect_err("shared ldq full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn shared_stq_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.shared_stq.queue_capacity = 2;
        config.resources.address_entries = 16;
        config.resources.store_data_entries = 2;

        let mut lsu = LsuSubgraph::new(config, 1);
        for _ in 0..2 {
            let req = SmemRequest::new(0, 16, 0xF, true, 0);
            assert!(lsu.issue_smem(0, req).is_ok());
        }
        let req = SmemRequest::new(0, 16, 0xF, true, 0);
        let err = lsu.issue_smem(0, req).expect_err("shared stq full");
        assert_eq!(err.reason, LsuRejectReason::QueueFull);
    }

    #[test]
    fn load_data_entries_exactly_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.resources.load_data_entries = 2;
        let mut lsu = LsuSubgraph::new(config, 1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let payload = LsuPayload::Gmem(req);
        assert!(lsu.reserve_load_data(&payload));
        assert!(lsu.reserve_load_data(&payload));
        assert!(!lsu.can_reserve_load_data(&payload));
    }

    #[test]
    fn all_warps_all_queues_full() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.queues.global_ldq.queue_capacity = 1;
        config.queues.global_stq.queue_capacity = 1;
        config.queues.shared_ldq.queue_capacity = 1;
        config.queues.shared_stq.queue_capacity = 1;
        config.resources.address_entries = 16;
        config.resources.store_data_entries = 16;

        let mut lsu = LsuSubgraph::new(config, 2);
        for warp in 0..2 {
            assert!(lsu.issue_gmem(0, GmemRequest::new(warp, 4, 0xF, true)).is_ok());
            assert!(lsu.issue_gmem(0, GmemRequest::new(warp, 4, 0xF, false)).is_ok());
            assert!(lsu.issue_smem(0, SmemRequest::new(warp, 4, 0xF, false, 0)).is_ok());
            assert!(lsu.issue_smem(0, SmemRequest::new(warp, 4, 0xF, true, 0)).is_ok());
        }
        let err = lsu
            .issue_gmem(0, GmemRequest::new(0, 4, 0xF, true))
            .expect_err("queues should be full or busy");
        assert!(
            matches!(err.reason, LsuRejectReason::QueueFull | LsuRejectReason::Busy),
            "unexpected reject reason: {:?}",
            err.reason
        );
    }

    #[test]
    fn rapid_issue_complete_cycle() {
        let mut config = LsuFlowConfig::default();
        config.issue = default_issue_config();
        config.link_capacity = 4;
        let mut lsu = LsuSubgraph::new(config, 1);
        let mut issued = 0u64;
        for cycle in 0..200 {
            let req = GmemRequest::new(0, 4, 0xF, true);
            if lsu.issue_gmem(cycle, req).is_ok() {
                issued += 1;
            }
            lsu.tick(cycle);
            while lsu.take_ready(cycle).is_some() {}
        }
        assert!(issued > 0);
        assert!(lsu.stats().completed > 0);
    }
}
