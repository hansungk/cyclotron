use std::collections::VecDeque;

#[path = "gmem_cache.rs"]
mod gmem_cache;
#[path = "gmem_mshr.rs"]
mod gmem_mshr;
#[path = "gmem_policy.rs"]
mod gmem_policy;

use crate::timeflow::graph::{FlowGraph, Link};
use crate::timeflow::server_node::ServerNode;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};
use gmem_cache::CacheTagArray;
use gmem_mshr::{MissLevel, MissMetadata, MshrAdmission, MshrTable};
pub use gmem_policy::GmemPolicyConfig;
use gmem_policy::{bank_for, decide, line_addr};
use serde::Deserialize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmemRequestKind {
    Load,
    Store,
    FlushL0,
    FlushL1,
}

impl GmemRequestKind {
    pub fn is_mem(self) -> bool {
        matches!(self, Self::Load | Self::Store)
    }

    pub fn is_flush_l0(self) -> bool {
        matches!(self, Self::FlushL0)
    }

    pub fn is_flush_l1(self) -> bool {
        matches!(self, Self::FlushL1)
    }
}

#[derive(Debug, Clone)]
pub struct GmemRequest {
    pub id: u64,
    pub core_id: usize,
    pub cluster_id: usize,
    pub warp: usize,
    pub addr: u64,
    pub line_addr: u64,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_load: bool,
    pub stall_on_completion: bool,
    pub kind: GmemRequestKind,
    pub l0_hit: bool,
    pub l1_hit: bool,
    pub l2_hit: bool,
    pub l1_writeback: bool,
    pub l2_writeback: bool,
    pub l1_bank: usize,
    pub l2_bank: usize,
}

impl GmemRequest {
    pub fn new(warp: usize, bytes: u32, active_lanes: u32, is_load: bool) -> Self {
        let kind = if is_load {
            GmemRequestKind::Load
        } else {
            GmemRequestKind::Store
        };
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            bytes,
            active_lanes,
            is_load,
            stall_on_completion: is_load,
            kind,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
        }
    }

    pub fn new_flush_l0(warp: usize, bytes: u32) -> Self {
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            bytes,
            active_lanes: 0,
            is_load: false,
            stall_on_completion: true,
            kind: GmemRequestKind::FlushL0,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
        }
    }

    pub fn new_flush_l1(warp: usize, bytes: u32) -> Self {
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            bytes,
            active_lanes: 0,
            is_load: false,
            stall_on_completion: true,
            kind: GmemRequestKind::FlushL1,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
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

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LinkConfig {
    pub entries: usize,
    #[serde(default)]
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
pub struct GmemNodeConfig {
    #[serde(default)]
    pub coalescer: ServerConfig,
    #[serde(default)]
    pub l0_flush_gate: ServerConfig,
    #[serde(default)]
    pub l0d_tag: ServerConfig,
    #[serde(default)]
    pub l0d_data: ServerConfig,
    #[serde(default)]
    pub l0d_mshr: ServerConfig,
    #[serde(default)]
    pub l1_flush_gate: ServerConfig,
    #[serde(default)]
    pub l1_tag: ServerConfig,
    #[serde(default)]
    pub l1_data: ServerConfig,
    #[serde(default)]
    pub l1_mshr: ServerConfig,
    #[serde(default)]
    pub l1_refill: ServerConfig,
    #[serde(default)]
    pub l1_writeback: ServerConfig,
    #[serde(default)]
    pub l2_tag: ServerConfig,
    #[serde(default)]
    pub l2_data: ServerConfig,
    #[serde(default)]
    pub l2_mshr: ServerConfig,
    #[serde(default)]
    pub l2_refill: ServerConfig,
    #[serde(default)]
    pub l2_writeback: ServerConfig,
    #[serde(default)]
    pub dram: ServerConfig,
    #[serde(default)]
    pub return_path: ServerConfig,
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
pub struct GmemLinkConfig {
    #[serde(default)]
    pub default: LinkConfig,
    #[serde(default)]
    pub coalescer_to_l0_flush: Option<LinkConfig>,
    #[serde(default)]
    pub l0_flush_to_l0_tag: Option<LinkConfig>,
    #[serde(default)]
    pub l0_flush_to_return: Option<LinkConfig>,
    #[serde(default)]
    pub l0_flush_to_l1_flush: Option<LinkConfig>,
    #[serde(default)]
    pub l0_tag_to_l0_hit: Option<LinkConfig>,
    #[serde(default)]
    pub l0_tag_to_l0_mshr: Option<LinkConfig>,
    #[serde(default)]
    pub l0_hit_to_return: Option<LinkConfig>,
    #[serde(default)]
    pub l0_mshr_to_l1_flush: Option<LinkConfig>,
    #[serde(default)]
    pub l1_flush_to_l1_tag: Option<LinkConfig>,
    #[serde(default)]
    pub l1_flush_to_return: Option<LinkConfig>,
    #[serde(default)]
    pub l1_tag_to_l1_hit: Option<LinkConfig>,
    #[serde(default)]
    pub l1_tag_to_l1_mshr: Option<LinkConfig>,
    #[serde(default)]
    pub l1_hit_to_return: Option<LinkConfig>,
    #[serde(default)]
    pub l1_mshr_to_l1_writeback: Option<LinkConfig>,
    #[serde(default)]
    pub l1_mshr_to_l2_tag: Option<LinkConfig>,
    #[serde(default)]
    pub l1_writeback_to_l2_tag: Option<LinkConfig>,
    #[serde(default)]
    pub l2_tag_to_l2_hit: Option<LinkConfig>,
    #[serde(default)]
    pub l2_tag_to_l2_mshr: Option<LinkConfig>,
    #[serde(default)]
    pub l2_hit_to_l1_refill: Option<LinkConfig>,
    #[serde(default)]
    pub l2_mshr_to_l2_writeback: Option<LinkConfig>,
    #[serde(default)]
    pub l2_mshr_to_dram: Option<LinkConfig>,
    #[serde(default)]
    pub l2_writeback_to_dram: Option<LinkConfig>,
    #[serde(default)]
    pub dram_to_l2_refill: Option<LinkConfig>,
    #[serde(default)]
    pub l2_refill_to_l1_refill: Option<LinkConfig>,
    #[serde(default)]
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
pub struct GmemFlowConfig {
    #[serde(default)]
    pub nodes: GmemNodeConfig,
    #[serde(default)]
    pub links: GmemLinkConfig,
    #[serde(default)]
    pub policy: GmemPolicyConfig,
}

impl Default for GmemFlowConfig {
    fn default() -> Self {
        Self {
            nodes: GmemNodeConfig::default(),
            links: GmemLinkConfig::default(),
            policy: GmemPolicyConfig::default(),
        }
    }
}

pub(crate) struct GmemSubgraph {
    ingress_node: NodeId,
    return_node: NodeId,
    pub(crate) completions: VecDeque<GmemCompletion>,
    next_id: u64,
    pub(crate) stats: GmemStats,
}

impl GmemSubgraph {
    pub fn attach(graph: &mut FlowGraph<CoreFlowPayload>, config: &GmemFlowConfig) -> Self {
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
        let l0_data =
            graph.add_node(ServerNode::new("l0d_data", TimedServer::new(nodes.l0d_data)));
        let l0_mshr =
            graph.add_node(ServerNode::new("l0d_mshr", TimedServer::new(nodes.l0d_mshr)));
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

        let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

        graph.connect(
            ingress_node,
            l0_flush_gate,
            "coalescer->l0_flush",
            link(links.coalescer_to_l0_flush),
        );

        graph.connect_filtered(
            l0_flush_gate,
            return_node,
            "l0_flush->return",
            link(links.l0_flush_to_return),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l0()),
        );

        graph.connect_filtered(
            l0_flush_gate,
            l1_flush_gate,
            "l0_flush->l1_flush",
            link(links.l0_flush_to_l1_flush),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1()),
        );

        graph.connect_filtered(
            l0_flush_gate,
            l0_tag,
            "l0_flush->l0_tag",
            link(links.l0_flush_to_l0_tag),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_mem()),
        );

        graph.connect_filtered(
            l0_tag,
            l0_data,
            "l0_tag->l0_hit",
            link(links.l0_tag_to_l0_hit),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.l0_hit),
        );
        graph.connect_filtered(
            l0_tag,
            l0_mshr,
            "l0_tag->l0_mshr",
            link(links.l0_tag_to_l0_mshr),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if !req.l0_hit),
        );

        graph.connect(
            l0_data,
            return_node,
            "l0_hit->return",
            link(links.l0_hit_to_return),
        );
        graph.connect(
            l0_mshr,
            l1_flush_gate,
            "l0_mshr->l1_flush",
            link(links.l0_mshr_to_l1_flush),
        );

        graph.connect_filtered(
            l1_flush_gate,
            return_node,
            "l1_flush->return",
            link(links.l1_flush_to_return),
            |payload| matches!(payload, CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1()),
        );

        for bank in 0..l1_banks {
            let tag_node = l1_tag_nodes[bank];
            let data_node = l1_data_nodes[bank];
            let mshr_node = l1_mshr_nodes[bank];
            let refill_node = l1_refill_nodes[bank];
            let wb_node = l1_wb_nodes[bank];

            graph.connect_filtered(
                l1_flush_gate,
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
                return_node,
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
                let l2_tag = l2_tag_nodes[l2_bank];
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
                return_node,
                format!("l1_refill_{bank}->return"),
                link(links.l1_refill_to_return),
            );
        }

        for bank in 0..l2_banks {
            let tag_node = l2_tag_nodes[bank];
            let data_node = l2_data_nodes[bank];
            let mshr_node = l2_mshr_nodes[bank];
            let refill_node = l2_refill_nodes[bank];
            let wb_node = l2_wb_nodes[bank];

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

            for l1_bank in 0..l1_banks {
                let l1_refill = l1_refill_nodes[l1_bank];
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

        for l2_bank in 0..l2_banks {
            let refill_node = l2_refill_nodes[l2_bank];
            graph.connect_filtered(
                dram_node,
                refill_node,
                format!("dram->l2_refill_{l2_bank}"),
                link(links.dram_to_l2_refill),
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == l2_bank)
                },
            );
        }

        Self {
            ingress_node,
            return_node,
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

pub struct ClusterGmemGraph {
    graph: FlowGraph<CoreFlowPayload>,
    cores: Vec<ClusterCoreState>,
    policy: GmemPolicyConfig,
    l0_tags: Vec<CacheTagArray>,
    l1_tags: Vec<CacheTagArray>,
    l2_tags: CacheTagArray,
    l0_mshrs: Vec<MshrTable>,
    l1_mshrs: Vec<Vec<MshrTable>>,
    l2_mshrs: Vec<MshrTable>,
    l0_mshr_admit: Vec<MshrAdmission>,
    l1_mshr_admit: Vec<Vec<MshrAdmission>>,
    l2_mshr_admit: Vec<MshrAdmission>,
    l1_banks: usize,
    l2_banks: usize,
    last_tick: Cycle,
}

struct ClusterCoreState {
    ingress_node: NodeId,
    return_node: NodeId,
    completions: VecDeque<GmemCompletion>,
    stats: GmemStats,
    next_id: u64,
}

impl ClusterGmemGraph {
    pub fn new(config: GmemFlowConfig, num_clusters: usize, cores_per_cluster: usize) -> Self {
        let mut graph = FlowGraph::new();
        let nodes = &config.nodes;
        let links = &config.links;
        let l1_banks = config.policy.l1_banks.max(1);
        let l2_banks = config.policy.l2_banks.max(1);

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

        let link = |cfg: Option<LinkConfig>| cfg.unwrap_or(links.default).build();

        for bank in 0..l2_banks {
            let tag_node = l2_tag_nodes[bank];
            let data_node = l2_data_nodes[bank];
            let mshr_node = l2_mshr_nodes[bank];
            let refill_node = l2_refill_nodes[bank];
            let wb_node = l2_wb_nodes[bank];

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
                move |payload| {
                    matches!(payload, CoreFlowPayload::Gmem(req) if req.l2_bank == bank)
                },
            );
        }

        struct ClusterL1State {
            l1_flush_gate: NodeId,
            l1_tag_nodes: Vec<NodeId>,
            l1_data_nodes: Vec<NodeId>,
            l1_mshr_nodes: Vec<NodeId>,
            l1_refill_nodes: Vec<NodeId>,
            l1_wb_nodes: Vec<NodeId>,
        }

        let mut cluster_l1 = Vec::with_capacity(num_clusters);
        for cluster_id in 0..num_clusters {
            let l1_flush_gate = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_l1_flush_gate"),
                TimedServer::new(nodes.l1_flush_gate),
            ));

            let mut l1_tag_nodes = Vec::with_capacity(l1_banks);
            let mut l1_data_nodes = Vec::with_capacity(l1_banks);
            let mut l1_mshr_nodes = Vec::with_capacity(l1_banks);
            let mut l1_refill_nodes = Vec::with_capacity(l1_banks);
            let mut l1_wb_nodes = Vec::with_capacity(l1_banks);

            for bank in 0..l1_banks {
                l1_tag_nodes.push(graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_l1_tag_{bank}"),
                    TimedServer::new(nodes.l1_tag),
                )));
                l1_data_nodes.push(graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_l1_data_{bank}"),
                    TimedServer::new(nodes.l1_data),
                )));
                l1_mshr_nodes.push(graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_l1_mshr_{bank}"),
                    TimedServer::new(nodes.l1_mshr),
                )));
                l1_refill_nodes.push(graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_l1_refill_{bank}"),
                    TimedServer::new(nodes.l1_refill),
                )));
                l1_wb_nodes.push(graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_l1_wb_{bank}"),
                    TimedServer::new(nodes.l1_writeback),
                )));
            }

            for bank in 0..l1_banks {
                let tag_node = l1_tag_nodes[bank];
                let data_node = l1_data_nodes[bank];
                let mshr_node = l1_mshr_nodes[bank];
                let wb_node = l1_wb_nodes[bank];

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

            cluster_l1.push(ClusterL1State {
                l1_flush_gate,
                l1_tag_nodes,
                l1_data_nodes,
                l1_mshr_nodes,
                l1_refill_nodes,
                l1_wb_nodes,
            });
        }

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
                        format!(
                            "l2_refill_{l2_bank}->cluster{cluster_id}_l1_refill_{l1_bank}"
                        ),
                        link(links.l2_refill_to_l1_refill),
                        move |payload| {
                            matches!(payload, CoreFlowPayload::Gmem(req)
                                if req.cluster_id == cluster_id && req.l1_bank == l1_bank)
                        },
                    );
                }
            }
        }

        let total_cores = num_clusters.saturating_mul(cores_per_cluster);
        let mut cores = Vec::with_capacity(total_cores);

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
                    TimedServer::new(nodes.l0d_tag),
                ));
                let l0_data = graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_core{local_core}_l0d_data"),
                    TimedServer::new(nodes.l0d_data),
                ));
                let l0_mshr = graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_core{local_core}_l0d_mshr"),
                    TimedServer::new(nodes.l0d_mshr),
                ));

                let return_node = graph.add_node(ServerNode::new(
                    format!("cluster{cluster_id}_core{local_core}_return"),
                    TimedServer::new(nodes.return_path),
                ));

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

                cores.push(ClusterCoreState {
                    ingress_node,
                    return_node,
                    completions: VecDeque::new(),
                    stats: GmemStats::default(),
                    next_id: 0,
                });
            }
        }

        let policy = config.policy;
        let l0_sets = policy.l0_sets.max(1);
        let l0_ways = policy.l0_ways.max(1);
        let l1_sets = policy.l1_sets.max(1);
        let l1_ways = policy.l1_ways.max(1);
        let l2_sets = policy.l2_sets.max(1);
        let l2_ways = policy.l2_ways.max(1);

        let mut l0_tags = Vec::with_capacity(total_cores);
        for _ in 0..total_cores {
            l0_tags.push(CacheTagArray::new(l0_sets, l0_ways));
        }

        let mut l1_tags = Vec::with_capacity(num_clusters);
        for _ in 0..num_clusters {
            l1_tags.push(CacheTagArray::new(l1_sets, l1_ways));
        }

        let l2_tags = CacheTagArray::new(l2_sets, l2_ways);

        let l0_mshr_capacity = nodes.l0d_mshr.queue_capacity;
        let l1_mshr_capacity = nodes.l1_mshr.queue_capacity;
        let l2_mshr_capacity = nodes.l2_mshr.queue_capacity;

        let mut l0_mshrs = Vec::with_capacity(total_cores);
        for _ in 0..total_cores {
            l0_mshrs.push(MshrTable::new(l0_mshr_capacity));
        }

        let mut l1_mshrs = Vec::with_capacity(num_clusters);
        for _ in 0..num_clusters {
            let mut banks = Vec::with_capacity(l1_banks);
            for _ in 0..l1_banks {
                banks.push(MshrTable::new(l1_mshr_capacity));
            }
            l1_mshrs.push(banks);
        }

        let mut l2_mshrs = Vec::with_capacity(l2_banks);
        for _ in 0..l2_banks {
            l2_mshrs.push(MshrTable::new(l2_mshr_capacity));
        }

        let mut l0_mshr_admit = Vec::with_capacity(total_cores);
        for _ in 0..total_cores {
            l0_mshr_admit.push(MshrAdmission::new(nodes.l0d_mshr));
        }

        let mut l1_mshr_admit = Vec::with_capacity(num_clusters);
        for _ in 0..num_clusters {
            let mut banks = Vec::with_capacity(l1_banks);
            for _ in 0..l1_banks {
                banks.push(MshrAdmission::new(nodes.l1_mshr));
            }
            l1_mshr_admit.push(banks);
        }

        let mut l2_mshr_admit = Vec::with_capacity(l2_banks);
        for _ in 0..l2_banks {
            l2_mshr_admit.push(MshrAdmission::new(nodes.l2_mshr));
        }

        Self {
            graph,
            cores,
            policy,
            l0_tags,
            l1_tags,
            l2_tags,
            l0_mshrs,
            l1_mshrs,
            l2_mshrs,
            l0_mshr_admit,
            l1_mshr_admit,
            l2_mshr_admit,
            l1_banks,
            l2_banks,
            last_tick: u64::MAX,
        }
    }

    pub fn issue(
        &mut self,
        core_id: usize,
        now: Cycle,
        mut request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        if core_id >= self.cores.len() {
            return Err(GmemReject {
                request,
                retry_at: now.saturating_add(1),
                reason: GmemRejectReason::QueueFull,
            });
        }
        request.core_id = core_id;
        let assigned_id = if request.id == 0 {
            self.cores[core_id].next_id
        } else {
            request.id
        };
        request.id = assigned_id;

        if !request.kind.is_mem() {
            request.bytes = self.policy.flush_bytes.max(1);
            request.l0_hit = false;
            request.l1_hit = false;
            request.l2_hit = false;
            request.l1_writeback = false;
            request.l2_writeback = false;
            request.l1_bank = 0;
            request.l2_bank = 0;
            request.line_addr = 0;
            return self.issue_to_graph(core_id, now, request);
        }

        let cluster_id = request.cluster_id;
        if cluster_id >= self.l1_tags.len() {
            let retry_at = now.saturating_add(1);
            return Err(GmemReject {
                request,
                retry_at,
                reason: GmemRejectReason::QueueFull,
            });
        }

        let policy = self.policy;
        let l0_line = line_addr(request.addr, policy.l0_line_bytes);
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        request.line_addr = l2_line;
        let l1_banks = self.l1_banks.max(1) as u64;
        let l2_banks = self.l2_banks.max(1) as u64;
        request.l1_bank = bank_for(l1_line, l1_banks, 0x1111_2222_3333_4444);
        request.l2_bank = bank_for(l2_line, l2_banks, 0x5555_6666_7777_8888);

        let l0_hit = { self.l0_tags[core_id].probe(l0_line) };
        request.l0_hit = l0_hit;
        if l0_hit {
            request.l1_hit = false;
            request.l2_hit = false;
            request.l1_writeback = false;
            request.l2_writeback = false;
        } else {
            let l1_hit = { self.l1_tags[cluster_id].probe(l1_line) };
            request.l1_hit = l1_hit;
            if l1_hit {
                request.l2_hit = false;
            } else {
                request.l2_hit = self.l2_tags.probe(l2_line);
            }

            if !request.l1_hit {
                let l1_key = l1_line
                    ^ (cluster_id as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
                    ^ policy.seed;
                request.l1_writeback =
                    decide(policy.l1_writeback_rate, l1_key ^ 0xD4D4_D4D4_D4D4_D4D4);
            } else {
                request.l1_writeback = false;
            }

            if !request.l1_hit && !request.l2_hit {
                let l2_key = l2_line ^ policy.seed;
                request.l2_writeback =
                    decide(policy.l2_writeback_rate, l2_key ^ 0xE5E5_E5E5_E5E5_E5E5);
            } else {
                request.l2_writeback = false;
            }
        }

        let miss_level = if request.l0_hit {
            MissLevel::None
        } else if request.l1_hit {
            MissLevel::L0
        } else if request.l2_hit {
            MissLevel::L1
        } else {
            MissLevel::L2
        };

        let meta = MissMetadata::from_request(&request);
        let l1_bank = request.l1_bank;
        let l2_bank = request.l2_bank;
        let mut l0_new = false;
        let mut l1_new = false;
        let mut l2_new = false;

        match miss_level {
            MissLevel::None => {}
            MissLevel::L0 => {
                if self.l0_mshrs[core_id].has_entry(l0_line) {
                    let bytes = request.bytes;
                    let ready_at = self
                        .l0_mshrs[core_id]
                        .merge_request(l0_line, request)
                        .unwrap_or(now.saturating_add(1));
                    return Ok(self.issue_merge(
                        core_id,
                        assigned_id,
                        now,
                        ready_at,
                        bytes,
                    ));
                }
                if !self.l0_mshrs[core_id].can_allocate(l0_line) {
                    self.cores[core_id].stats.queue_full_rejects += 1;
                    return Err(GmemReject {
                        request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l0_mshr_admit[core_id].try_admit(now) {
                    return Err(self.reject_for_admission(core_id, now, &request, bp));
                }
                l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.cores[core_id].stats.queue_full_rejects += 1;
                        return Err(GmemReject {
                            request,
                            retry_at: now.saturating_add(1),
                            reason: GmemRejectReason::QueueFull,
                        });
                    }
                };
            }
            MissLevel::L1 => {
                if !self.l0_mshrs[core_id].has_entry(l0_line) {
                    l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
                        Ok(new_entry) => new_entry,
                        Err(_) => {
                            self.cores[core_id].stats.queue_full_rejects += 1;
                            return Err(GmemReject {
                                request,
                                retry_at: now.saturating_add(1),
                                reason: GmemRejectReason::QueueFull,
                            });
                        }
                    };
                }

                if self.l1_mshrs[cluster_id][l1_bank].has_entry(l1_line) {
                    let bytes = request.bytes;
                    let ready_at = self
                        .l1_mshrs[cluster_id][l1_bank]
                        .merge_request(l1_line, request)
                        .unwrap_or(now.saturating_add(1));
                    self.l0_mshrs[core_id].set_ready_at(l0_line, ready_at);
                    return Ok(self.issue_merge(
                        core_id,
                        assigned_id,
                        now,
                        ready_at,
                        bytes,
                    ));
                }

                if !self.l1_mshrs[cluster_id][l1_bank].can_allocate(l1_line) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    self.cores[core_id].stats.queue_full_rejects += 1;
                    return Err(GmemReject {
                        request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l1_mshr_admit[cluster_id][l1_bank].try_admit(now) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    return Err(self.reject_for_admission(core_id, now, &request, bp));
                }
                l1_new = match self.l1_mshrs[cluster_id][l1_bank].ensure_entry(l1_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.rollback_mshrs(
                            core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                            l0_new, l1_new, l2_new,
                        );
                        self.cores[core_id].stats.queue_full_rejects += 1;
                        return Err(GmemReject {
                            request,
                            retry_at: now.saturating_add(1),
                            reason: GmemRejectReason::QueueFull,
                        });
                    }
                };
            }
            MissLevel::L2 => {
                if !self.l0_mshrs[core_id].has_entry(l0_line) {
                    l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
                        Ok(new_entry) => new_entry,
                        Err(_) => {
                            self.cores[core_id].stats.queue_full_rejects += 1;
                            return Err(GmemReject {
                                request,
                                retry_at: now.saturating_add(1),
                                reason: GmemRejectReason::QueueFull,
                            });
                        }
                    };
                }

                if !self.l1_mshrs[cluster_id][l1_bank].has_entry(l1_line) {
                    l1_new = match self.l1_mshrs[cluster_id][l1_bank].ensure_entry(l1_line, meta) {
                        Ok(new_entry) => new_entry,
                        Err(_) => {
                            self.rollback_mshrs(
                                core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line,
                                l2_line, l0_new, l1_new, l2_new,
                            );
                            self.cores[core_id].stats.queue_full_rejects += 1;
                            return Err(GmemReject {
                                request,
                                retry_at: now.saturating_add(1),
                                reason: GmemRejectReason::QueueFull,
                            });
                        }
                    };
                }

                if self.l2_mshrs[l2_bank].has_entry(l2_line) {
                    let bytes = request.bytes;
                    let ready_at = self
                        .l2_mshrs[l2_bank]
                        .merge_request(l2_line, request)
                        .unwrap_or(now.saturating_add(1));
                    self.l1_mshrs[cluster_id][l1_bank].set_ready_at(l1_line, ready_at);
                    self.l0_mshrs[core_id].set_ready_at(l0_line, ready_at);
                    return Ok(self.issue_merge(
                        core_id,
                        assigned_id,
                        now,
                        ready_at,
                        bytes,
                    ));
                }

                if !self.l2_mshrs[l2_bank].can_allocate(l2_line) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    self.cores[core_id].stats.queue_full_rejects += 1;
                    return Err(GmemReject {
                        request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l2_mshr_admit[l2_bank].try_admit(now) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    return Err(self.reject_for_admission(core_id, now, &request, bp));
                }
                l2_new = match self.l2_mshrs[l2_bank].ensure_entry(l2_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.rollback_mshrs(
                            core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                            l0_new, l1_new, l2_new,
                        );
                        self.cores[core_id].stats.queue_full_rejects += 1;
                        return Err(GmemReject {
                            request,
                            retry_at: now.saturating_add(1),
                            reason: GmemRejectReason::QueueFull,
                        });
                    }
                };
            }
        }

        let issue = match self.issue_to_graph(core_id, now, request) {
            Ok(issue) => issue,
            Err(err) => {
                self.rollback_mshrs(
                    core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line, l0_new,
                    l1_new, l2_new,
                );
                return Err(err);
            }
        };

        let ready_at = issue.ticket.ready_at();
        match miss_level {
            MissLevel::None => {}
            MissLevel::L0 => {
                self.l0_mshrs[core_id].set_ready_at(l0_line, ready_at);
            }
            MissLevel::L1 => {
                self.l0_mshrs[core_id].set_ready_at(l0_line, ready_at);
                self.l1_mshrs[cluster_id][l1_bank].set_ready_at(l1_line, ready_at);
            }
            MissLevel::L2 => {
                self.l0_mshrs[core_id].set_ready_at(l0_line, ready_at);
                self.l1_mshrs[cluster_id][l1_bank].set_ready_at(l1_line, ready_at);
                self.l2_mshrs[l2_bank].set_ready_at(l2_line, ready_at);
            }
        }

        Ok(issue)
    }

    pub fn tick(&mut self, now: Cycle) {
        if now == self.last_tick {
            return;
        }
        self.last_tick = now;

        for admit in &mut self.l0_mshr_admit {
            admit.tick(now);
        }
        for cluster in &mut self.l1_mshr_admit {
            for admit in cluster {
                admit.tick(now);
            }
        }
        for admit in &mut self.l2_mshr_admit {
            admit.tick(now);
        }

        self.graph.tick(now);

        for core_id in 0..self.cores.len() {
            let return_node = self.cores[core_id].return_node;
            let mut drained = Vec::new();
            self.graph.with_node_mut(return_node, |node| {
                while let Some(result) = node.take_ready(now) {
                    if let CoreFlowPayload::Gmem(request) = result.payload {
                        drained.push((request, result.ticket));
                    }
                }
            });

            for (request, ticket) in drained {
                let ticket_ready_at = ticket.ready_at();
                self.push_completion(request.clone(), ticket_ready_at, now);
                self.apply_completion_effects(&request);

                let merged = self.drain_mshr_merges(&request);
                for merged_req in merged {
                    self.push_completion(merged_req.clone(), ticket_ready_at, now);
                    self.apply_completion_effects(&merged_req);
                }
            }
        }
    }

    fn issue_to_graph(
        &mut self,
        core_id: usize,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        let request_id = request.id;
        let bytes = request.bytes;
        let payload = CoreFlowPayload::Gmem(request);
        let service_req = ServiceRequest::new(payload, bytes);

        match self.graph.try_put(self.cores[core_id].ingress_node, now, service_req) {
            Ok(ticket) => {
                self.record_issue_stats(core_id, request_id, bytes);
                Ok(GmemIssue { request_id, ticket })
            }
            Err(bp) => match bp {
                Backpressure::Busy {
                    request,
                    available_at,
                } => {
                    let stats = &mut self.cores[core_id].stats;
                    stats.busy_rejects += 1;
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
                    let stats = &mut self.cores[core_id].stats;
                    stats.queue_full_rejects += 1;
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

    fn reject_for_admission(
        &mut self,
        core_id: usize,
        now: Cycle,
        request: &GmemRequest,
        bp: Backpressure<()>,
    ) -> GmemReject {
        let stats = &mut self.cores[core_id].stats;
        match bp {
            Backpressure::Busy { available_at, .. } => {
                stats.busy_rejects += 1;
                let mut retry_at = available_at;
                if retry_at <= now {
                    retry_at = now.saturating_add(1);
                }
                GmemReject {
                    request: request.clone(),
                    retry_at,
                    reason: GmemRejectReason::Busy,
                }
            }
            Backpressure::QueueFull { .. } => {
                stats.queue_full_rejects += 1;
                let retry_at = now.saturating_add(1);
                GmemReject {
                    request: request.clone(),
                    retry_at,
                    reason: GmemRejectReason::QueueFull,
                }
            }
        }
    }

    fn issue_merge(
        &mut self,
        core_id: usize,
        request_id: u64,
        issued_at: Cycle,
        ready_at: Cycle,
        bytes: u32,
    ) -> GmemIssue {
        self.record_issue_stats(core_id, request_id, bytes);
        let ticket = Ticket::synthetic(issued_at, ready_at, bytes);
        GmemIssue { request_id, ticket }
    }

    fn record_issue_stats(&mut self, core_id: usize, request_id: u64, bytes: u32) {
        if let Some(core_state) = self.cores.get_mut(core_id) {
            if request_id >= core_state.next_id {
                core_state.next_id = request_id.saturating_add(1);
            }
            let stats = &mut core_state.stats;
            stats.issued = stats.issued.saturating_add(1);
            stats.bytes_issued = stats.bytes_issued.saturating_add(bytes as u64);
            stats.inflight = stats.inflight.saturating_add(1);
            stats.max_inflight = stats.max_inflight.max(stats.inflight);
        }
    }

    fn push_completion(&mut self, request: GmemRequest, ticket_ready_at: Cycle, now: Cycle) {
        let core_id = request.core_id;
        if let Some(core_state) = self.cores.get_mut(core_id) {
            core_state.stats.completed = core_state.stats.completed.saturating_add(1);
            core_state.stats.bytes_completed = core_state
                .stats
                .bytes_completed
                .saturating_add(request.bytes as u64);
            core_state.stats.inflight = core_state.stats.inflight.saturating_sub(1);
            core_state.stats.last_completion_cycle = Some(now);
            core_state.completions.push_back(GmemCompletion {
                ticket_ready_at,
                completed_at: now,
                request,
            });
            core_state.stats.max_completion_queue = core_state
                .stats
                .max_completion_queue
                .max(core_state.completions.len() as u64);
        }
    }

    fn apply_completion_effects(&mut self, request: &GmemRequest) {
        if request.kind.is_flush_l0() {
            if request.core_id < self.l0_tags.len() {
                self.l0_tags[request.core_id].invalidate_all();
            }
            return;
        }
        if request.kind.is_flush_l1() {
            if request.cluster_id < self.l1_tags.len() {
                self.l1_tags[request.cluster_id].invalidate_all();
            }
            return;
        }
        if !request.kind.is_mem() {
            return;
        }

        let policy = self.policy;
        let l0_line = line_addr(request.addr, policy.l0_line_bytes);
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        if !request.l0_hit && request.core_id < self.l0_tags.len() {
            self.l0_tags[request.core_id].fill(l0_line);
        }
        if !request.l1_hit && request.cluster_id < self.l1_tags.len() {
            self.l1_tags[request.cluster_id].fill(l1_line);
        }
        if !request.l2_hit {
            self.l2_tags.fill(l2_line);
        }
    }

    fn drain_mshr_merges(&mut self, request: &GmemRequest) -> Vec<GmemRequest> {
        if !request.kind.is_mem() {
            return Vec::new();
        }

        let miss_level = if request.l0_hit {
            MissLevel::None
        } else if request.l1_hit {
            MissLevel::L0
        } else if request.l2_hit {
            MissLevel::L1
        } else {
            MissLevel::L2
        };

        if miss_level == MissLevel::None {
            return Vec::new();
        }

        let policy = self.policy;
        let l0_line = line_addr(request.addr, policy.l0_line_bytes);
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        match miss_level {
            MissLevel::L0 => self.l0_mshrs[request.core_id]
                .remove_entry(l0_line)
                .map(|entry| entry.merged)
                .unwrap_or_default(),
            MissLevel::L1 => {
                let cluster_id = request.cluster_id;
                let l1_bank = request.l1_bank;
                let merged = if cluster_id < self.l1_mshrs.len()
                    && l1_bank < self.l1_mshrs[cluster_id].len()
                {
                    self.l1_mshrs[cluster_id][l1_bank]
                        .remove_entry(l1_line)
                        .map(|entry| entry.merged)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let mut all = Vec::with_capacity(merged.len() + 1);
                all.push(request.clone());
                all.extend(merged.iter().cloned());
                for req in &all {
                    if req.core_id < self.l0_mshrs.len() {
                        let l0_line_req = line_addr(req.addr, policy.l0_line_bytes);
                        self.l0_mshrs[req.core_id].remove_entry(l0_line_req);
                    }
                }
                merged
            }
            MissLevel::L2 => {
                let l2_bank = request.l2_bank;
                let merged = if l2_bank < self.l2_mshrs.len() {
                    self.l2_mshrs[l2_bank]
                        .remove_entry(l2_line)
                        .map(|entry| entry.merged)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let mut all = Vec::with_capacity(merged.len() + 1);
                all.push(request.clone());
                all.extend(merged.iter().cloned());
                for req in &all {
                    if req.core_id < self.l0_mshrs.len() {
                        let l0_line_req = line_addr(req.addr, policy.l0_line_bytes);
                        self.l0_mshrs[req.core_id].remove_entry(l0_line_req);
                    }
                    let cluster_id = req.cluster_id;
                    if cluster_id < self.l1_mshrs.len() {
                        let l1_line_req = line_addr(req.addr, policy.l1_line_bytes);
                        let l1_bank_req = req.l1_bank.min(self.l1_banks.saturating_sub(1));
                        if l1_bank_req < self.l1_mshrs[cluster_id].len() {
                            self.l1_mshrs[cluster_id][l1_bank_req]
                                .remove_entry(l1_line_req);
                        }
                    }
                }
                merged
            }
            MissLevel::None => Vec::new(),
        }
    }

    fn rollback_mshrs(
        &mut self,
        core_id: usize,
        cluster_id: usize,
        l1_bank: usize,
        l2_bank: usize,
        l0_line: u64,
        l1_line: u64,
        l2_line: u64,
        l0_new: bool,
        l1_new: bool,
        l2_new: bool,
    ) {
        if l0_new && core_id < self.l0_mshrs.len() {
            self.l0_mshrs[core_id].remove_entry(l0_line);
        }
        if l1_new
            && cluster_id < self.l1_mshrs.len()
            && l1_bank < self.l1_mshrs[cluster_id].len()
        {
            self.l1_mshrs[cluster_id][l1_bank].remove_entry(l1_line);
        }
        if l2_new && l2_bank < self.l2_mshrs.len() {
            self.l2_mshrs[l2_bank].remove_entry(l2_line);
        }
    }

    pub fn pop_completion(&mut self, core_id: usize) -> Option<GmemCompletion> {
        self.cores.get_mut(core_id)?.completions.pop_front()
    }

    pub fn pending_completions(&self, core_id: usize) -> usize {
        self.cores
            .get(core_id)
            .map(|core| core.completions.len())
            .unwrap_or(0)
    }

    pub fn stats(&self, core_id: usize) -> GmemStats {
        self.cores
            .get(core_id)
            .map(|core| core.stats)
            .unwrap_or_default()
    }

    pub fn clear_stats(&mut self, core_id: usize) {
        if let Some(core) = self.cores.get_mut(core_id) {
            core.stats = GmemStats::default();
        }
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

    fn fast_config() -> GmemFlowConfig {
        let mut cfg = GmemFlowConfig::default();
        let mut tune = |node: &mut ServerConfig| {
            node.base_latency = 0;
            node.bytes_per_cycle = 1024;
            node.queue_capacity = 8;
        };
        tune(&mut cfg.nodes.coalescer);
        tune(&mut cfg.nodes.l0_flush_gate);
        tune(&mut cfg.nodes.l0d_tag);
        tune(&mut cfg.nodes.l0d_data);
        tune(&mut cfg.nodes.l0d_mshr);
        tune(&mut cfg.nodes.l1_flush_gate);
        tune(&mut cfg.nodes.l1_tag);
        tune(&mut cfg.nodes.l1_data);
        tune(&mut cfg.nodes.l1_mshr);
        tune(&mut cfg.nodes.l1_refill);
        tune(&mut cfg.nodes.l1_writeback);
        tune(&mut cfg.nodes.l2_tag);
        tune(&mut cfg.nodes.l2_data);
        tune(&mut cfg.nodes.l2_mshr);
        tune(&mut cfg.nodes.l2_refill);
        tune(&mut cfg.nodes.l2_writeback);
        tune(&mut cfg.nodes.dram);
        tune(&mut cfg.nodes.return_path);
        cfg.links.default.entries = 8;
        cfg
    }

    fn make_load(addr: u64, cluster_id: usize) -> GmemRequest {
        let mut req = GmemRequest::new(0, 16, 0xF, true);
        req.addr = addr;
        req.cluster_id = cluster_id;
        req
    }

    fn complete_one(
        cluster: &mut ClusterGmemGraph,
        core_id: usize,
        start: Cycle,
        max_cycles: u64,
    ) -> GmemCompletion {
        let mut cycle = start;
        for _ in 0..max_cycles {
            cluster.tick(cycle);
            if let Some(completion) = cluster.pop_completion(core_id) {
                return completion;
            }
            cycle = cycle.saturating_add(1);
        }
        panic!("completion did not arrive within {} cycles", max_cycles);
    }

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
        for cycle in now..=ready_at.saturating_add(500) {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
        }
        assert_eq!(1, subgraph.completions.len());
    }

    #[test]
    fn gmem_backpressure_is_reported() {
        let mut cfg = GmemFlowConfig::default();
        cfg.nodes.coalescer.queue_capacity = 1;
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = GmemSubgraph::attach(&mut graph, &cfg);

        let req0 = GmemRequest::new(0, 16, 0xF, true);
        subgraph.issue(&mut graph, 0, req0).unwrap();
        let req1 = GmemRequest::new(0, 16, 0xF, true);
        let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
        assert_eq!(GmemRejectReason::QueueFull, err.reason);
    }

    #[test]
    fn gmem_stats_track_activity() {
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = GmemSubgraph::attach(&mut graph, &GmemFlowConfig::default());
        let req0 = GmemRequest::new(0, 16, 0xF, true);
        let issue = subgraph.issue(&mut graph, 0, req0).unwrap();
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(500) {
            graph.tick(cycle);
            subgraph.collect_completions(&mut graph, cycle);
        }
        let stats = subgraph.stats;
        assert_eq!(1, stats.issued);
        assert_eq!(1, stats.completed);
        assert_eq!(16, stats.bytes_issued);
        assert_eq!(16, stats.bytes_completed);
        assert_eq!(0, stats.inflight);
        assert!(stats.max_inflight >= 1);
    }

    #[test]
    fn gmem_allows_overlapping_requests() {
        let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
        let mut subgraph = GmemSubgraph::attach(&mut graph, &GmemFlowConfig::default());
        let req0 = GmemRequest::new(0, 16, 0xF, true);
        let issue0 = subgraph.issue(&mut graph, 0, req0).unwrap();
        let req1 = GmemRequest::new(0, 16, 0xF, true);
        let issue1 = subgraph.issue(&mut graph, 1, req1).unwrap();
        assert!(issue1.ticket.ready_at() > issue0.ticket.ready_at());
        assert!(
            issue1.ticket.ready_at() - issue0.ticket.ready_at()
                < GmemFlowConfig::default().nodes.coalescer.base_latency + 50
        );
    }

    #[test]
    fn cross_core_l2_merge_accepts_second_request() {
        let mut cfg = fast_config();
        cfg.nodes.l2_mshr.queue_capacity = 1;
        let mut cluster = ClusterGmemGraph::new(cfg, 2, 1);
        let now = 0;

        let req0 = make_load(0x1000, 0);
        let req1 = make_load(0x1000, 1);
        let issue0 = cluster.issue(0, now, req0).expect("first request accepts");
        let issue1 = cluster.issue(1, now, req1).expect("second request merges at L2");
        assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

        let comp0 = complete_one(&mut cluster, 0, now, 200);
        let comp1 = complete_one(&mut cluster, 1, now, 200);
        assert_eq!(comp0.completed_at, comp1.completed_at);
    }

    #[test]
    fn cross_core_l1_merge_accepts_second_request() {
        let mut cfg = fast_config();
        cfg.nodes.l1_mshr.queue_capacity = 1;
        let mut cluster = ClusterGmemGraph::new(cfg, 1, 2);
        let mut cycle = 0;

        let req0 = make_load(0x2000, 0);
        cluster
            .issue(0, cycle, req0)
            .expect("warmup request should accept");
        complete_one(&mut cluster, 0, cycle, 200);

        let mut flush_l0 = GmemRequest::new_flush_l0(0, 1);
        flush_l0.cluster_id = 0;
        cluster
            .issue(0, cycle, flush_l0)
            .expect("flush l0 should accept");
        let mut flush_l1 = GmemRequest::new_flush_l1(0, 1);
        flush_l1.cluster_id = 0;
        cluster
            .issue(0, cycle, flush_l1)
            .expect("flush l1 should accept");

        let mut got_l0 = false;
        let mut got_l1 = false;
        for _ in 0..200 {
            cluster.tick(cycle);
            while let Some(comp) = cluster.pop_completion(0) {
                if comp.request.kind.is_flush_l0() {
                    got_l0 = true;
                }
                if comp.request.kind.is_flush_l1() {
                    got_l1 = true;
                }
            }
            if got_l0 && got_l1 {
                break;
            }
            cycle = cycle.saturating_add(1);
        }
        assert!(got_l0 && got_l1, "expected both flush completions");

        let req0 = make_load(0x2000, 0);
        let req1 = make_load(0x2000, 0);
        let issue0 = cluster.issue(0, cycle, req0).expect("first post-flush load");
        let issue1 = cluster.issue(1, cycle, req1).expect("second load merges at L1");
        assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

        let comp0 = complete_one(&mut cluster, 0, cycle, 200);
        let comp1 = complete_one(&mut cluster, 1, cycle, 200);
        assert_eq!(comp0.completed_at, comp1.completed_at);
    }

    #[test]
    fn l0_flush_invalidates_only_l0() {
        let cfg = fast_config();
        let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
        let mut cycle = 0;

        let req0 = make_load(0x3000, 0);
        cluster.issue(0, cycle, req0).unwrap();
        complete_one(&mut cluster, 0, cycle, 200);

        let mut flush_l0 = GmemRequest::new_flush_l0(0, 1);
        flush_l0.cluster_id = 0;
        cluster.issue(0, cycle, flush_l0).unwrap();
        complete_one(&mut cluster, 0, cycle, 200);

        let req1 = make_load(0x3000, 0);
        cluster.issue(0, cycle, req1).unwrap();
        let comp = complete_one(&mut cluster, 0, cycle, 200);
        assert!(!comp.request.l0_hit, "expected L0 miss after flush");
        assert!(comp.request.l1_hit, "expected L1 hit after L0 flush");
    }

    #[test]
    fn l1_flush_invalidates_cluster_l1() {
        let cfg = fast_config();
        let mut cluster = ClusterGmemGraph::new(cfg, 1, 2);
        let mut cycle = 0;

        let req0 = make_load(0x4000, 0);
        cluster.issue(0, cycle, req0).unwrap();
        complete_one(&mut cluster, 0, cycle, 200);

        let mut flush_l1 = GmemRequest::new_flush_l1(0, 1);
        flush_l1.cluster_id = 0;
        cluster.issue(0, cycle, flush_l1).unwrap();
        complete_one(&mut cluster, 0, cycle, 200);

        let req1 = make_load(0x4000, 0);
        cluster.issue(1, cycle, req1).unwrap();
        let comp = complete_one(&mut cluster, 1, cycle, 200);
        assert!(!comp.request.l1_hit, "expected L1 miss after flush");
        assert!(comp.request.l2_hit, "expected L2 hit after L1 flush");
    }

    #[test]
    fn merge_completion_fanout_same_core() {
        let mut cfg = fast_config();
        cfg.nodes.l2_mshr.queue_capacity = 1;
        let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
        let now = 0;

        let req0 = make_load(0x5000, 0);
        let req1 = make_load(0x5000, 0);
        let issue0 = cluster.issue(0, now, req0).unwrap();
        let issue1 = cluster.issue(0, now, req1).unwrap();
        assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

        let comp0 = complete_one(&mut cluster, 0, now, 200);
        let comp1 = complete_one(&mut cluster, 0, now, 200);
        assert_eq!(comp0.completed_at, comp1.completed_at);
    }

    #[test]
    fn mshr_full_rejects_with_retry_cycle() {
        let mut cfg = fast_config();
        cfg.nodes.l2_mshr.queue_capacity = 1;
        cfg.nodes.l1_mshr.queue_capacity = 2;
        cfg.nodes.l0d_mshr.queue_capacity = 2;
        let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
        let now = 0;

        let req0 = make_load(0x6000, 0);
        cluster.issue(0, now, req0).unwrap();
        let req1 = make_load(0x8000, 0);
        let err = cluster.issue(0, now, req1).expect_err("MSHR should be full");
        assert_eq!(GmemRejectReason::QueueFull, err.reason);
        assert!(err.retry_at > now);
    }

}
