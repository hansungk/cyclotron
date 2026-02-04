use serde::Deserialize;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{ServerConfig, TimedServer};

use super::policy::GmemPolicyConfig;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LinkConfig {
    pub entries: usize,
    pub bytes: Option<u32>,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            entries: 16,
            bytes: None,
        }
    }
}

impl LinkConfig {
    fn build<T>(&self) -> Link<T> {
        match self.bytes {
            Some(limit) => Link::with_byte_limit(self.entries, Some(limit)),
            None => Link::new(self.entries),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmemNodeConfig {
    pub coalescer: ServerConfig,
    pub l0_flush_gate: ServerConfig,
    pub l0d_tag: ServerConfig,
    pub l0d_data: ServerConfig,
    pub l0d_mshr: ServerConfig,
    pub l1_flush_gate: ServerConfig,
    pub l1_tag: ServerConfig,
    pub l1_data: ServerConfig,
    pub l1_mshr: ServerConfig,
    pub l1_refill: ServerConfig,
    pub l1_writeback: ServerConfig,
    pub l2_tag: ServerConfig,
    pub l2_data: ServerConfig,
    pub l2_mshr: ServerConfig,
    pub l2_refill: ServerConfig,
    pub l2_writeback: ServerConfig,
    pub dram: ServerConfig,
    pub return_path: ServerConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheLevelConfig {
    pub name: String,
    pub banks: usize,
    pub tag: ServerConfig,
    pub data: ServerConfig,
    pub mshr: ServerConfig,
    pub refill: ServerConfig,
    pub writeback: ServerConfig,
}

impl Default for CacheLevelConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            banks: 1,
            tag: ServerConfig::default(),
            data: ServerConfig::default(),
            mshr: ServerConfig::default(),
            refill: ServerConfig::default(),
            writeback: ServerConfig::default(),
        }
    }
}

impl CacheLevelConfig {
    fn from_legacy(
        name: &str,
        banks: usize,
        tag: ServerConfig,
        data: ServerConfig,
        mshr: ServerConfig,
        refill: ServerConfig,
        writeback: ServerConfig,
    ) -> Self {
        Self {
            name: name.to_string(),
            banks,
            tag,
            data,
            mshr,
            refill,
            writeback,
        }
    }
}

impl Default for GmemNodeConfig {
    fn default() -> Self {
        Self {
            coalescer: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 8,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l0_flush_gate: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l0d_tag: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l0d_data: ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l0d_mshr: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l1_flush_gate: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l1_tag: ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l1_data: ServerConfig {
                base_latency: 6,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l1_mshr: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l1_refill: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l1_writeback: ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 32,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            l2_tag: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l2_data: ServerConfig {
                base_latency: 6,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l2_mshr: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l2_refill: ServerConfig {
                base_latency: 8,
                bytes_per_cycle: 32,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            l2_writeback: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            dram: ServerConfig {
                base_latency: 200,
                bytes_per_cycle: 32,
                queue_capacity: 64,
                ..ServerConfig::default()
            },
            return_path: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 1024,
                queue_capacity: 128,
                ..ServerConfig::default()
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmemLinkConfig {
    pub default: LinkConfig,
    pub coalescer_to_l0_flush: Option<LinkConfig>,
    pub l0_flush_to_l0_tag: Option<LinkConfig>,
    pub l0_flush_to_return: Option<LinkConfig>,
    pub l0_flush_to_l1_flush: Option<LinkConfig>,
    pub l0_tag_to_l0_hit: Option<LinkConfig>,
    pub l0_tag_to_l0_mshr: Option<LinkConfig>,
    pub l0_hit_to_return: Option<LinkConfig>,
    pub l0_mshr_to_l1_flush: Option<LinkConfig>,
    pub l1_flush_to_l1_tag: Option<LinkConfig>,
    pub l1_flush_to_return: Option<LinkConfig>,
    pub l1_tag_to_l1_hit: Option<LinkConfig>,
    pub l1_tag_to_l1_mshr: Option<LinkConfig>,
    pub l1_hit_to_return: Option<LinkConfig>,
    pub l1_mshr_to_l1_writeback: Option<LinkConfig>,
    pub l1_mshr_to_l2_tag: Option<LinkConfig>,
    pub l1_writeback_to_l2_tag: Option<LinkConfig>,
    pub l2_tag_to_l2_hit: Option<LinkConfig>,
    pub l2_tag_to_l2_mshr: Option<LinkConfig>,
    pub l2_hit_to_l1_refill: Option<LinkConfig>,
    pub l2_mshr_to_l2_writeback: Option<LinkConfig>,
    pub l2_mshr_to_dram: Option<LinkConfig>,
    pub l2_writeback_to_dram: Option<LinkConfig>,
    pub dram_to_l2_refill: Option<LinkConfig>,
    pub l2_refill_to_l1_refill: Option<LinkConfig>,
    pub l1_refill_to_return: Option<LinkConfig>,
}

impl Default for GmemLinkConfig {
    fn default() -> Self {
        Self {
            default: LinkConfig::default(),
            coalescer_to_l0_flush: None,
            l0_flush_to_l0_tag: None,
            l0_flush_to_return: None,
            l0_flush_to_l1_flush: None,
            l0_tag_to_l0_hit: None,
            l0_tag_to_l0_mshr: None,
            l0_hit_to_return: None,
            l0_mshr_to_l1_flush: None,
            l1_flush_to_l1_tag: None,
            l1_flush_to_return: None,
            l1_tag_to_l1_hit: None,
            l1_tag_to_l1_mshr: None,
            l1_hit_to_return: None,
            l1_mshr_to_l1_writeback: None,
            l1_mshr_to_l2_tag: None,
            l1_writeback_to_l2_tag: None,
            l2_tag_to_l2_hit: None,
            l2_tag_to_l2_mshr: None,
            l2_hit_to_l1_refill: None,
            l2_mshr_to_l2_writeback: None,
            l2_mshr_to_dram: None,
            l2_writeback_to_dram: None,
            dram_to_l2_refill: None,
            l2_refill_to_l1_refill: None,
            l1_refill_to_return: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmemFlowConfig {
    pub nodes: GmemNodeConfig,
    pub links: GmemLinkConfig,
    pub policy: GmemPolicyConfig,
    pub levels: Vec<CacheLevelConfig>,
}

impl Default for GmemFlowConfig {
    fn default() -> Self {
        Self {
            nodes: GmemNodeConfig::default(),
            links: GmemLinkConfig::default(),
            policy: GmemPolicyConfig::default(),
            levels: Vec::new(),
        }
    }
}

impl GmemFlowConfig {
    /// Test helper: a compact, high-throughput config used by unit tests.
    /// Keeps the same initialization behavior as the previous local
    /// `fast_config()` used in tests.
    pub fn zeroed() -> Self {
        let mut cfg = Self::default();
        let mut nodes = [
            &mut cfg.nodes.coalescer,
            &mut cfg.nodes.l0_flush_gate,
            &mut cfg.nodes.l0d_tag,
            &mut cfg.nodes.l0d_data,
            &mut cfg.nodes.l0d_mshr,
            &mut cfg.nodes.l1_flush_gate,
            &mut cfg.nodes.l1_tag,
            &mut cfg.nodes.l1_data,
            &mut cfg.nodes.l1_mshr,
            &mut cfg.nodes.l1_refill,
            &mut cfg.nodes.l1_writeback,
            &mut cfg.nodes.l2_tag,
            &mut cfg.nodes.l2_data,
            &mut cfg.nodes.l2_mshr,
            &mut cfg.nodes.l2_refill,
            &mut cfg.nodes.l2_writeback,
            &mut cfg.nodes.dram,
            &mut cfg.nodes.return_path,
        ];
        for node in &mut nodes {
            node.base_latency = 0;
            node.bytes_per_cycle = 1024;
            node.queue_capacity = 8;
        }
        cfg.links.default.entries = 8;
        cfg
    }

    pub fn cache_levels(&self) -> Vec<CacheLevelConfig> {
        if !self.levels.is_empty() {
            return self.levels.clone();
        }
        vec![
            CacheLevelConfig::from_legacy(
                "l0",
                1,
                self.nodes.l0d_tag,
                self.nodes.l0d_data,
                self.nodes.l0d_mshr,
                self.nodes.l0d_mshr,
                self.nodes.l0d_mshr,
            ),
            CacheLevelConfig::from_legacy(
                "l1",
                self.policy.l1_banks,
                self.nodes.l1_tag,
                self.nodes.l1_data,
                self.nodes.l1_mshr,
                self.nodes.l1_refill,
                self.nodes.l1_writeback,
            ),
            CacheLevelConfig::from_legacy(
                "l2",
                self.policy.l2_banks,
                self.nodes.l2_tag,
                self.nodes.l2_data,
                self.nodes.l2_mshr,
                self.nodes.l2_refill,
                self.nodes.l2_writeback,
            ),
        ]
    }
}

pub(crate) struct CoreGraphNodes {
    pub(crate) ingress_node: NodeId,
    pub(crate) return_node: NodeId,
}

pub(crate) struct ClusterCoreNodes {
    pub(crate) ingress_node: NodeId,
    pub(crate) return_node: NodeId,
}

struct CoreGraphState {
    ingress_node: NodeId,
    l0_flush_gate: NodeId,
    l0_tag: NodeId,
    l0_data: NodeId,
    l0_mshr: NodeId,
    l1_flush_gate: NodeId,
    l1_tag_nodes: Vec<NodeId>,
    l1_data_nodes: Vec<NodeId>,
    l1_mshr_nodes: Vec<NodeId>,
    l1_refill_nodes: Vec<NodeId>,
    l1_wb_nodes: Vec<NodeId>,
    l2_tag_nodes: Vec<NodeId>,
    l2_data_nodes: Vec<NodeId>,
    l2_mshr_nodes: Vec<NodeId>,
    l2_refill_nodes: Vec<NodeId>,
    l2_wb_nodes: Vec<NodeId>,
    dram_node: NodeId,
    return_node: NodeId,
}

fn connect_core_l0(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    state: &CoreGraphState,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

    graph.connect(
        state.ingress_node,
        state.l0_flush_gate,
        "coalescer->l0_flush",
        link(links.coalescer_to_l0_flush),
    );

    graph.connect_filtered(
        state.l0_flush_gate,
        state.return_node,
        "l0_flush->return",
        link(links.l0_flush_to_return),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l0()),
    );

    graph.connect_filtered(
        state.l0_flush_gate,
        state.l1_flush_gate,
        "l0_flush->l1_flush",
        link(links.l0_flush_to_l1_flush),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1()),
    );

    graph.connect_filtered(
        state.l0_flush_gate,
        state.l0_tag,
        "l0_flush->l0_tag",
        link(links.l0_flush_to_l0_tag),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_mem()),
    );

    graph.connect_filtered(
        state.l0_tag,
        state.l0_data,
        "l0_tag->l0_hit",
        link(links.l0_tag_to_l0_hit),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l0_hit),
    );
    graph.connect_filtered(
        state.l0_tag,
        state.l0_mshr,
        "l0_tag->l0_mshr",
        link(links.l0_tag_to_l0_mshr),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l0_hit),
    );

    graph.connect(
        state.l0_data,
        state.return_node,
        "l0_hit->return",
        link(links.l0_hit_to_return),
    );
    graph.connect(
        state.l0_mshr,
        state.l1_flush_gate,
        "l0_mshr->l1_flush",
        link(links.l0_mshr_to_l1_flush),
    );
}

fn connect_core_no_l0(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    state: &CoreGraphState,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();
    graph.connect_filtered(
        state.ingress_node,
        state.return_node,
        "coalescer->return_flush_l0",
        link(links.coalescer_to_l0_flush),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l0()),
    );
    graph.connect_filtered(
        state.ingress_node,
        state.l1_flush_gate,
        "coalescer->l1_flush",
        link(links.coalescer_to_l0_flush),
        |payload| {
            matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_mem() || req.kind.is_flush_l1())
        },
    );
}

fn connect_core_l1(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    state: &CoreGraphState,
    l1_banks: usize,
    l2_banks: usize,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

    graph.connect_filtered(
        state.l1_flush_gate,
        state.return_node,
        "l1_flush->return",
        link(links.l1_flush_to_return),
        |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1()),
    );

    for bank in 0..l1_banks {
        let tag_node = state.l1_tag_nodes[bank];
        let data_node = state.l1_data_nodes[bank];
        let mshr_node = state.l1_mshr_nodes[bank];
        let refill_node = state.l1_refill_nodes[bank];
        let wb_node = state.l1_wb_nodes[bank];

        graph.connect_filtered(
            state.l1_flush_gate,
            tag_node,
            format!("l1_flush->l1_tag_{bank}"),
            link(links.l1_flush_to_l1_tag),
            move |payload| {
                matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_mem() && req.l1_bank == bank)
            },
        );

        graph.connect_filtered(
            tag_node,
            data_node,
            format!("l1_tag_{bank}->l1_hit_{bank}"),
            link(links.l1_tag_to_l1_hit),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_hit),
        );
        graph.connect_filtered(
            tag_node,
            mshr_node,
            format!("l1_tag_{bank}->l1_mshr_{bank}"),
            link(links.l1_tag_to_l1_mshr),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l1_hit),
        );

        graph.connect(
            data_node,
            state.return_node,
            format!("l1_hit_{bank}->return"),
            link(links.l1_hit_to_return),
        );

        graph.connect_filtered(
            mshr_node,
            wb_node,
            format!("l1_mshr_{bank}->l1_wb_{bank}"),
            link(links.l1_mshr_to_l1_writeback),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_writeback),
        );

        for l2_bank in 0..l2_banks {
            let l2_tag = state.l2_tag_nodes[l2_bank];
            graph.connect_filtered(
                mshr_node,
                l2_tag,
                format!("l1_mshr_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_mshr_to_l2_tag),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if !req.l1_writeback && req.l2_bank == l2_bank)
                },
            );
            graph.connect_filtered(
                wb_node,
                l2_tag,
                format!("l1_wb_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_writeback_to_l2_tag),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == l2_bank)
                },
            );
        }

        graph.connect(
            refill_node,
            state.return_node,
            format!("l1_refill_{bank}->return"),
            link(links.l1_refill_to_return),
        );
    }
}

fn connect_core_l2(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    state: &CoreGraphState,
    l1_banks: usize,
    l2_banks: usize,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

    for bank in 0..l2_banks {
        let tag_node = state.l2_tag_nodes[bank];
        let data_node = state.l2_data_nodes[bank];
        let mshr_node = state.l2_mshr_nodes[bank];
        let refill_node = state.l2_refill_nodes[bank];
        let wb_node = state.l2_wb_nodes[bank];

        graph.connect_filtered(
            tag_node,
            data_node,
            format!("l2_tag_{bank}->l2_hit_{bank}"),
            link(links.l2_tag_to_l2_hit),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_hit),
        );
        graph.connect_filtered(
            tag_node,
            mshr_node,
            format!("l2_tag_{bank}->l2_mshr_{bank}"),
            link(links.l2_tag_to_l2_mshr),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l2_hit),
        );

        graph.connect_filtered(
            mshr_node,
            wb_node,
            format!("l2_mshr_{bank}->l2_wb_{bank}"),
            link(links.l2_mshr_to_l2_writeback),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_writeback),
        );
        graph.connect_filtered(
            mshr_node,
            state.dram_node,
            format!("l2_mshr_{bank}->dram"),
            link(links.l2_mshr_to_dram),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l2_writeback),
        );
        graph.connect(
            wb_node,
            state.dram_node,
            format!("l2_wb_{bank}->dram"),
            link(links.l2_writeback_to_dram),
        );

        for l1_bank in 0..l1_banks {
            let l1_refill = state.l1_refill_nodes[l1_bank];
            graph.connect_filtered(
                data_node,
                l1_refill,
                format!("l2_hit_{bank}->l1_refill_{l1_bank}"),
                link(links.l2_hit_to_l1_refill),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_bank == l1_bank)
                },
            );
            graph.connect_filtered(
                refill_node,
                l1_refill,
                format!("l2_refill_{bank}->l1_refill_{l1_bank}"),
                link(links.l2_refill_to_l1_refill),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_bank == l1_bank)
                },
            );
        }
    }
}

fn connect_core_dram(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    state: &CoreGraphState,
    l2_banks: usize,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

    for l2_bank in 0..l2_banks {
        let refill_node = state.l2_refill_nodes[l2_bank];
        graph.connect_filtered(
            state.dram_node,
            refill_node,
            format!("dram->l2_refill_{l2_bank}"),
            link(links.dram_to_l2_refill),
            move |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == l2_bank),
        );
    }
}

struct ClusterL1State {
    l1_flush_gate: NodeId,
    l1_data_nodes: Vec<NodeId>,
    l1_refill_nodes: Vec<NodeId>,
}

struct CacheLevelNodes {
    tag_nodes: Vec<NodeId>,
    data_nodes: Vec<NodeId>,
    mshr_nodes: Vec<NodeId>,
    refill_nodes: Vec<NodeId>,
    wb_nodes: Vec<NodeId>,
}

fn build_cache_level_nodes(
    graph: &mut FlowGraph<CoreFlowPayload>,
    prefix: &str,
    level: &CacheLevelConfig,
) -> CacheLevelNodes {
    let banks = level.banks.max(1);
    let mut tag_nodes = Vec::with_capacity(banks);
    let mut data_nodes = Vec::with_capacity(banks);
    let mut mshr_nodes = Vec::with_capacity(banks);
    let mut refill_nodes = Vec::with_capacity(banks);
    let mut wb_nodes = Vec::with_capacity(banks);
    for bank in 0..banks {
        tag_nodes.push(graph.add_node(ServerNode::new(
            format!("{prefix}_tag_{bank}"),
            TimedServer::new(level.tag),
        )));
        data_nodes.push(graph.add_node(ServerNode::new(
            format!("{prefix}_data_{bank}"),
            TimedServer::new(level.data),
        )));
        mshr_nodes.push(graph.add_node(ServerNode::new(
            format!("{prefix}_mshr_{bank}"),
            TimedServer::new(level.mshr),
        )));
        refill_nodes.push(graph.add_node(ServerNode::new(
            format!("{prefix}_refill_{bank}"),
            TimedServer::new(level.refill),
        )));
        wb_nodes.push(graph.add_node(ServerNode::new(
            format!("{prefix}_wb_{bank}"),
            TimedServer::new(level.writeback),
        )));
    }
    CacheLevelNodes {
        tag_nodes,
        data_nodes,
        mshr_nodes,
        refill_nodes,
        wb_nodes,
    }
}

fn build_cluster_l2(
    graph: &mut FlowGraph<CoreFlowPayload>,
    nodes: &GmemNodeConfig,
    links: &GmemLinkConfig,
    level: &CacheLevelConfig,
) -> (
    Vec<NodeId>,
    Vec<NodeId>,
    Vec<NodeId>,
    Vec<NodeId>,
    Vec<NodeId>,
    NodeId,
) {
    let l2_nodes = build_cache_level_nodes(graph, "l2", level);
    let l2_banks = level.banks.max(1);

    let dram_node = graph.add_node(ServerNode::new("dram", TimedServer::new(nodes.dram)));
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

    for bank in 0..l2_banks {
        let tag_node = l2_nodes.tag_nodes[bank];
        let data_node = l2_nodes.data_nodes[bank];
        let mshr_node = l2_nodes.mshr_nodes[bank];
        let refill_node = l2_nodes.refill_nodes[bank];
        let wb_node = l2_nodes.wb_nodes[bank];

        graph.connect_filtered(
            tag_node,
            data_node,
            format!("l2_tag_{bank}->l2_hit_{bank}"),
            link(links.l2_tag_to_l2_hit),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_hit),
        );
        graph.connect_filtered(
            tag_node,
            mshr_node,
            format!("l2_tag_{bank}->l2_mshr_{bank}"),
            link(links.l2_tag_to_l2_mshr),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l2_hit),
        );

        graph.connect_filtered(
            mshr_node,
            wb_node,
            format!("l2_mshr_{bank}->l2_wb_{bank}"),
            link(links.l2_mshr_to_l2_writeback),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_writeback),
        );
        graph.connect_filtered(
            mshr_node,
            dram_node,
            format!("l2_mshr_{bank}->dram"),
            link(links.l2_mshr_to_dram),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l2_writeback),
        );
        graph.connect(
            wb_node,
            dram_node,
            format!("l2_wb_{bank}->dram"),
            link(links.l2_writeback_to_dram),
        );

        graph.connect_filtered(
            dram_node,
            refill_node,
            format!("dram->l2_refill_{bank}"),
            link(links.dram_to_l2_refill),
            move |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == bank),
        );
    }

    (
        l2_nodes.tag_nodes,
        l2_nodes.data_nodes,
        l2_nodes.mshr_nodes,
        l2_nodes.refill_nodes,
        l2_nodes.wb_nodes,
        dram_node,
    )
}

fn build_cluster_l1(
    graph: &mut FlowGraph<CoreFlowPayload>,
    nodes: &GmemNodeConfig,
    links: &GmemLinkConfig,
    level: &CacheLevelConfig,
    l2_banks: usize,
    cluster_id: usize,
    l2_tag_nodes: &[NodeId],
) -> ClusterL1State {
    let l1_flush_gate = graph.add_node(ServerNode::new(
        format!("cluster{cluster_id}_l1_flush_gate"),
        TimedServer::new(nodes.l1_flush_gate),
    ));

    let l1_nodes = build_cache_level_nodes(graph, &format!("cluster{cluster_id}_l1"), level);
    let l1_banks = level.banks.max(1);

    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();
    for bank in 0..l1_banks {
        let tag_node = l1_nodes.tag_nodes[bank];
        let data_node = l1_nodes.data_nodes[bank];
        let mshr_node = l1_nodes.mshr_nodes[bank];
        let wb_node = l1_nodes.wb_nodes[bank];

        graph.connect_filtered(
            l1_flush_gate,
            tag_node,
            format!("cluster{cluster_id}_l1_flush->l1_tag_{bank}"),
            link(links.l1_flush_to_l1_tag),
            move |payload| {
                matches!(payload, CoreFlowPayload::Gmem(req)
                    if req.kind.is_mem() && req.cluster_id == cluster_id && req.l1_bank == bank)
            },
        );

        graph.connect_filtered(
            tag_node,
            data_node,
            format!("cluster{cluster_id}_l1_tag_{bank}->l1_hit"),
            link(links.l1_tag_to_l1_hit),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_hit),
        );
        graph.connect_filtered(
            tag_node,
            mshr_node,
            format!("cluster{cluster_id}_l1_tag_{bank}->l1_mshr"),
            link(links.l1_tag_to_l1_mshr),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l1_hit),
        );

        graph.connect_filtered(
            mshr_node,
            wb_node,
            format!("cluster{cluster_id}_l1_mshr_{bank}->l1_wb"),
            link(links.l1_mshr_to_l1_writeback),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l1_writeback),
        );

        for l2_bank in 0..l2_banks {
            let l2_tag = l2_tag_nodes[l2_bank];
            graph.connect_filtered(
                mshr_node,
                l2_tag,
                format!("cluster{cluster_id}_l1_mshr_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_mshr_to_l2_tag),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req)
                        if !req.l1_writeback && req.l2_bank == l2_bank)
                },
            );
            graph.connect_filtered(
                wb_node,
                l2_tag,
                format!("cluster{cluster_id}_l1_wb_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_writeback_to_l2_tag),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == l2_bank)
                },
            );
        }
    }

    ClusterL1State {
        l1_flush_gate,
        l1_data_nodes: l1_nodes.data_nodes,
        l1_refill_nodes: l1_nodes.refill_nodes,
    }
}

fn connect_cluster_l2_to_l1_refills(
    graph: &mut FlowGraph<CoreFlowPayload>,
    links: &GmemLinkConfig,
    l2_data_nodes: &[NodeId],
    l2_refill_nodes: &[NodeId],
    cluster_l1: &[ClusterL1State],
    l1_banks: usize,
    l2_banks: usize,
    num_clusters: usize,
) {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();
    for cluster_id in 0..num_clusters {
        let l1_refill_nodes = &cluster_l1[cluster_id].l1_refill_nodes;
        for l1_bank in 0..l1_banks {
            let l1_refill = l1_refill_nodes[l1_bank];
            for l2_bank in 0..l2_banks {
                let l2_data = l2_data_nodes[l2_bank];
                let l2_refill = l2_refill_nodes[l2_bank];
                graph.connect_filtered(
                    l2_data,
                    l1_refill,
                    format!("l2_hit_{l2_bank}->cluster{cluster_id}_l1_refill_{l1_bank}"),
                    link(links.l2_hit_to_l1_refill),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.cluster_id == cluster_id && req.l1_bank == l1_bank)
                    },
                );
                graph.connect_filtered(
                    l2_refill,
                    l1_refill,
                    format!("l2_refill_{l2_bank}->cluster{cluster_id}_l1_refill_{l1_bank}"),
                    link(links.l2_refill_to_l1_refill),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.cluster_id == cluster_id && req.l1_bank == l1_bank)
                    },
                );
            }
        }
    }
}

fn build_cluster_core_nodes(
    graph: &mut FlowGraph<CoreFlowPayload>,
    nodes: &GmemNodeConfig,
    links: &GmemLinkConfig,
    l0_level: Option<&CacheLevelConfig>,
    cluster_l1: &[ClusterL1State],
    l1_banks: usize,
    num_clusters: usize,
    cores_per_cluster: usize,
    l0_enabled: bool,
) -> Vec<ClusterCoreNodes> {
    let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();
    let total_cores = num_clusters.saturating_mul(cores_per_cluster);
    let mut core_nodes = Vec::with_capacity(total_cores);

    for cluster_id in 0..num_clusters {
        let base_core = cluster_id.saturating_mul(cores_per_cluster);
        let cluster_state = &cluster_l1[cluster_id];
        for local_core in 0..cores_per_cluster {
            let core_id = base_core + local_core;
            let ingress_node = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_coalescer"),
                TimedServer::new(nodes.coalescer),
            ));
            let l0_flush_gate = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0_flush_gate"),
                TimedServer::new(nodes.l0_flush_gate),
            ));
            let l0_tag = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_tag"),
                TimedServer::new(l0_level.map(|level| level.tag).unwrap_or(nodes.l0d_tag)),
            ));
            let l0_data = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_data"),
                TimedServer::new(l0_level.map(|level| level.data).unwrap_or(nodes.l0d_data)),
            ));
            let l0_mshr = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_mshr"),
                TimedServer::new(l0_level.map(|level| level.mshr).unwrap_or(nodes.l0d_mshr)),
            ));

            let return_node = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_return"),
                TimedServer::new(nodes.return_path),
            ));

            if l0_enabled {
                graph.connect(
                    ingress_node,
                    l0_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_coalescer->l0_flush"),
                    link(links.coalescer_to_l0_flush),
                );

                let core_match = core_id;
                graph.connect_filtered(
                    l0_flush_gate,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->return"),
                    link(links.l0_flush_to_return),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.kind.is_flush_l0() && req.core_id == core_match)
                    },
                );
                graph.connect_filtered(
                    l0_flush_gate,
                    cluster_state.l1_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->l1_flush"),
                    link(links.l0_flush_to_l1_flush),
                    |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1()),
                );
                graph.connect_filtered(
                    l0_flush_gate,
                    l0_tag,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->l0_tag"),
                    link(links.l0_flush_to_l0_tag),
                    |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_mem()),
                );

                graph.connect_filtered(
                    l0_tag,
                    l0_data,
                    format!("cluster{cluster_id}_core{local_core}_l0_tag->l0_hit"),
                    link(links.l0_tag_to_l0_hit),
                    |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l0_hit),
                );
                graph.connect_filtered(
                    l0_tag,
                    l0_mshr,
                    format!("cluster{cluster_id}_core{local_core}_l0_tag->l0_mshr"),
                    link(links.l0_tag_to_l0_mshr),
                    |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l0_hit),
                );

                graph.connect(
                    l0_data,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l0_hit->return"),
                    link(links.l0_hit_to_return),
                );
                graph.connect(
                    l0_mshr,
                    cluster_state.l1_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_l0_mshr->l1_flush"),
                    link(links.l0_mshr_to_l1_flush),
                );
            } else {
                let core_match = core_id;
                graph.connect_filtered(
                    ingress_node,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_coalescer->return_flush_l0"),
                    link(links.coalescer_to_l0_flush),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.kind.is_flush_l0() && req.core_id == core_match)
                    },
                );
                graph.connect_filtered(
                    ingress_node,
                    cluster_state.l1_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_coalescer->l1_flush"),
                    link(links.coalescer_to_l0_flush),
                    |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.kind.is_mem() || req.kind.is_flush_l1())
                    },
                );
            }

            let core_match = core_id;
            graph.connect_filtered(
                cluster_state.l1_flush_gate,
                return_node,
                format!("cluster{cluster_id}_core{local_core}_l1_flush->return"),
                link(links.l1_flush_to_return),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req)
                        if req.kind.is_flush_l1() && req.core_id == core_match)
                },
            );

            for bank in 0..l1_banks {
                let l1_data = cluster_state.l1_data_nodes[bank];
                let l1_refill = cluster_state.l1_refill_nodes[bank];
                let core_match = core_id;
                graph.connect_filtered(
                    l1_data,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l1_hit_{bank}->return"),
                    link(links.l1_hit_to_return),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.core_id == core_match)
                    },
                );
                let core_match = core_id;
                graph.connect_filtered(
                    l1_refill,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l1_refill_{bank}->return"),
                    link(links.l1_refill_to_return),
                    move |payload| {
                        matches!(payload, CoreFlowPayload::Gmem(req)
                            if req.core_id == core_match)
                    },
                );
            }

            core_nodes.push(ClusterCoreNodes {
                ingress_node,
                return_node,
            });
        }
    }

    core_nodes
}

pub(crate) fn build_core_graph(
    graph: &mut FlowGraph<CoreFlowPayload>,
    config: &GmemFlowConfig,
) -> CoreGraphNodes {
    let nodes = &config.nodes;
    let links = &config.links;
    let l1_banks = config.policy.l1_banks.max(1);
    let l2_banks = config.policy.l2_banks.max(1);

    let ingress_node = graph.add_node(ServerNode::new(
        "coalescer",
        TimedServer::new(nodes.coalescer),
    ));
    let l0_flush_gate = graph.add_node(ServerNode::new(
        "l0_flush_gate",
        TimedServer::new(nodes.l0_flush_gate),
    ));
    let l0_tag = graph.add_node(ServerNode::new("l0d_tag", TimedServer::new(nodes.l0d_tag)));
    let l0_data = graph.add_node(ServerNode::new(
        "l0d_data",
        TimedServer::new(nodes.l0d_data),
    ));
    let l0_mshr = graph.add_node(ServerNode::new(
        "l0d_mshr",
        TimedServer::new(nodes.l0d_mshr),
    ));
    let l1_flush_gate = graph.add_node(ServerNode::new(
        "l1_flush_gate",
        TimedServer::new(nodes.l1_flush_gate),
    ));

    let mut l1_tag_nodes = Vec::with_capacity(l1_banks);
    let mut l1_data_nodes = Vec::with_capacity(l1_banks);
    let mut l1_mshr_nodes = Vec::with_capacity(l1_banks);
    let mut l1_refill_nodes = Vec::with_capacity(l1_banks);
    let mut l1_wb_nodes = Vec::with_capacity(l1_banks);
    for bank in 0..l1_banks {
        l1_tag_nodes.push(graph.add_node(ServerNode::new(
            format!("l1_tag_{bank}"),
            TimedServer::new(nodes.l1_tag),
        )));
        l1_data_nodes.push(graph.add_node(ServerNode::new(
            format!("l1_data_{bank}"),
            TimedServer::new(nodes.l1_data),
        )));
        l1_mshr_nodes.push(graph.add_node(ServerNode::new(
            format!("l1_mshr_{bank}"),
            TimedServer::new(nodes.l1_mshr),
        )));
        l1_refill_nodes.push(graph.add_node(ServerNode::new(
            format!("l1_refill_{bank}"),
            TimedServer::new(nodes.l1_refill),
        )));
        l1_wb_nodes.push(graph.add_node(ServerNode::new(
            format!("l1_wb_{bank}"),
            TimedServer::new(nodes.l1_writeback),
        )));
    }

    let mut l2_tag_nodes = Vec::with_capacity(l2_banks);
    let mut l2_data_nodes = Vec::with_capacity(l2_banks);
    let mut l2_mshr_nodes = Vec::with_capacity(l2_banks);
    let mut l2_refill_nodes = Vec::with_capacity(l2_banks);
    let mut l2_wb_nodes = Vec::with_capacity(l2_banks);
    for bank in 0..l2_banks {
        l2_tag_nodes.push(graph.add_node(ServerNode::new(
            format!("l2_tag_{bank}"),
            TimedServer::new(nodes.l2_tag),
        )));
        l2_data_nodes.push(graph.add_node(ServerNode::new(
            format!("l2_data_{bank}"),
            TimedServer::new(nodes.l2_data),
        )));
        l2_mshr_nodes.push(graph.add_node(ServerNode::new(
            format!("l2_mshr_{bank}"),
            TimedServer::new(nodes.l2_mshr),
        )));
        l2_refill_nodes.push(graph.add_node(ServerNode::new(
            format!("l2_refill_{bank}"),
            TimedServer::new(nodes.l2_refill),
        )));
        l2_wb_nodes.push(graph.add_node(ServerNode::new(
            format!("l2_wb_{bank}"),
            TimedServer::new(nodes.l2_writeback),
        )));
    }

    let dram_node = graph.add_node(ServerNode::new("dram", TimedServer::new(nodes.dram)));
    let return_node = graph.add_node(ServerNode::new(
        "gmem_return",
        TimedServer::new(nodes.return_path),
    ));

    let state = CoreGraphState {
        ingress_node,
        l0_flush_gate,
        l0_tag,
        l0_data,
        l0_mshr,
        l1_flush_gate,
        l1_tag_nodes,
        l1_data_nodes,
        l1_mshr_nodes,
        l1_refill_nodes,
        l1_wb_nodes,
        l2_tag_nodes,
        l2_data_nodes,
        l2_mshr_nodes,
        l2_refill_nodes,
        l2_wb_nodes,
        dram_node,
        return_node,
    };

    if config.policy.l0_enabled {
        connect_core_l0(graph, links, &state);
    } else {
        connect_core_no_l0(graph, links, &state);
    }
    connect_core_l1(graph, links, &state, l1_banks, l2_banks);
    connect_core_l2(graph, links, &state, l1_banks, l2_banks);
    connect_core_dram(graph, links, &state, l2_banks);

    CoreGraphNodes {
        ingress_node: state.ingress_node,
        return_node: state.return_node,
    }
}

pub(crate) fn build_cluster_graph(
    config: &GmemFlowConfig,
    num_clusters: usize,
    cores_per_cluster: usize,
) -> (FlowGraph<CoreFlowPayload>, Vec<ClusterCoreNodes>) {
    let mut graph = FlowGraph::new();
    let nodes = &config.nodes;
    let links = &config.links;
    let levels = config.cache_levels();
    let l0_level = levels.get(0);
    let l1_level = levels.get(1);
    let l2_level = levels.get(2);
    let l1_banks = l1_level
        .map(|level| level.banks.max(1))
        .unwrap_or_else(|| config.policy.l1_banks.max(1));
    let l2_banks = l2_level
        .map(|level| level.banks.max(1))
        .unwrap_or_else(|| config.policy.l2_banks.max(1));

    let l2_fallback = CacheLevelConfig::from_legacy(
        "l2",
        l2_banks,
        nodes.l2_tag,
        nodes.l2_data,
        nodes.l2_mshr,
        nodes.l2_refill,
        nodes.l2_writeback,
    );
    let l2_level = l2_level.unwrap_or(&l2_fallback);
    let (l2_tag_nodes, l2_data_nodes, _l2_mshr_nodes, l2_refill_nodes, _l2_wb_nodes, _dram) =
        build_cluster_l2(&mut graph, nodes, links, l2_level);

    let l1_fallback = CacheLevelConfig::from_legacy(
        "l1",
        l1_banks,
        nodes.l1_tag,
        nodes.l1_data,
        nodes.l1_mshr,
        nodes.l1_refill,
        nodes.l1_writeback,
    );
    let l1_level = l1_level.unwrap_or(&l1_fallback);
    let mut cluster_l1 = Vec::with_capacity(num_clusters);
    for cluster_id in 0..num_clusters {
        cluster_l1.push(build_cluster_l1(
            &mut graph,
            nodes,
            links,
            l1_level,
            l2_banks,
            cluster_id,
            &l2_tag_nodes,
        ));
    }

    connect_cluster_l2_to_l1_refills(
        &mut graph,
        links,
        &l2_data_nodes,
        &l2_refill_nodes,
        &cluster_l1,
        l1_banks,
        l2_banks,
        num_clusters,
    );

    let core_nodes = build_cluster_core_nodes(
        &mut graph,
        nodes,
        links,
        l0_level,
        &cluster_l1,
        l1_banks,
        num_clusters,
        cores_per_cluster,
        config.policy.l0_enabled && l0_level.is_some(),
    );

    (graph, core_nodes)
}
