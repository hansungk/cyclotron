use serde::Deserialize;

use crate::timeflow::{
    graph::{FlowGraph, Link},
    server_node::ServerNode,
    types::{CoreFlowPayload, NodeId},
};
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
    pub l1_flush_gate: ServerConfig,
    pub dram: ServerConfig,
    pub return_path: ServerConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheLevelConfig {
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
    fn new(
        banks: usize,
        tag: ServerConfig,
        data: ServerConfig,
        mshr: ServerConfig,
        refill: ServerConfig,
        writeback: ServerConfig,
    ) -> Self {
        Self {
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
            l1_flush_gate: ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 64,
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

fn default_levels() -> Vec<CacheLevelConfig> {
    vec![
        CacheLevelConfig::new(
            1,
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
        ),
        CacheLevelConfig::new(
            2,
            ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 6,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 2,
                bytes_per_cycle: 32,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
        ),
        CacheLevelConfig::new(
            1,
            ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 6,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 64,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 8,
                bytes_per_cycle: 32,
                queue_capacity: 16,
                ..ServerConfig::default()
            },
            ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 32,
                queue_capacity: 8,
                ..ServerConfig::default()
            },
        ),
    ]
}

impl Default for GmemFlowConfig {
    fn default() -> Self {
        Self {
            nodes: GmemNodeConfig::default(),
            links: GmemLinkConfig::default(),
            policy: GmemPolicyConfig::default(),
            levels: default_levels(),
        }
    }
}

impl GmemFlowConfig {
    pub fn zeroed() -> Self {
        let mut cfg = Self::default();
        for node in [
            &mut cfg.nodes.coalescer,
            &mut cfg.nodes.l0_flush_gate,
            &mut cfg.nodes.l1_flush_gate,
            &mut cfg.nodes.dram,
            &mut cfg.nodes.return_path,
        ] {
            node.base_latency = 0;
            node.bytes_per_cycle = 1024;
            node.queue_capacity = 8;
        }
        for level in &mut cfg.levels {
            for node in [
                &mut level.tag,
                &mut level.data,
                &mut level.mshr,
                &mut level.refill,
                &mut level.writeback,
            ] {
                node.base_latency = 0;
                node.bytes_per_cycle = 1024;
                node.queue_capacity = 8;
            }
        }
        cfg.links.default.entries = 8;
        cfg
    }
}

pub(crate) struct ClusterCoreNodes {
    pub(crate) ingress_node: NodeId,
    pub(crate) return_node: NodeId,
}

struct CacheLevelNodes {
    tag_nodes: Vec<NodeId>,
    data_nodes: Vec<NodeId>,
    mshr_nodes: Vec<NodeId>,
    refill_nodes: Vec<NodeId>,
    wb_nodes: Vec<NodeId>,
}

struct ClusterL1State {
    l1_flush_gate: NodeId,
    l1_data_nodes: Vec<NodeId>,
    l1_refill_nodes: Vec<NodeId>,
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

        graph.connect(
            tag_node,
            data_node,
            format!("l2_tag_{bank}->l2_hit_{bank}"),
            link(links.l2_tag_to_l2_hit),
        );
        graph.connect(
            tag_node,
            mshr_node,
            format!("l2_tag_{bank}->l2_mshr_{bank}"),
            link(links.l2_tag_to_l2_mshr),
        );
        graph.set_route_fn(tag_node, |payload| match payload {
            CoreFlowPayload::Gmem(req) if req.l2_hit => 0,
            _ => 1,
        });

        graph.connect(
            mshr_node,
            wb_node,
            format!("l2_mshr_{bank}->l2_wb_{bank}"),
            link(links.l2_mshr_to_l2_writeback),
        );
        graph.connect(
            mshr_node,
            dram_node,
            format!("l2_mshr_{bank}->dram"),
            link(links.l2_mshr_to_dram),
        );
        graph.set_route_fn(mshr_node, |payload| match payload {
            CoreFlowPayload::Gmem(req) if req.l2_writeback => 0,
            _ => 1,
        });
        graph.connect(
            wb_node,
            dram_node,
            format!("l2_wb_{bank}->dram"),
            link(links.l2_writeback_to_dram),
        );

        graph.connect(
            dram_node,
            refill_node,
            format!("dram->l2_refill_{bank}"),
            link(links.dram_to_l2_refill),
        );
    }
    graph.set_route_fn(dram_node, |payload| match payload {
        CoreFlowPayload::Gmem(req) => req.l2_bank,
        _ => 0,
    });

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
    cores_per_cluster: usize,
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

        graph.connect(
            l1_flush_gate,
            tag_node,
            format!("cluster{cluster_id}_l1_flush->l1_tag_{bank}"),
            link(links.l1_flush_to_l1_tag),
        );

        graph.connect(
            tag_node,
            data_node,
            format!("cluster{cluster_id}_l1_tag_{bank}->l1_hit"),
            link(links.l1_tag_to_l1_hit),
        );
        graph.connect(
            tag_node,
            mshr_node,
            format!("cluster{cluster_id}_l1_tag_{bank}->l1_mshr"),
            link(links.l1_tag_to_l1_mshr),
        );
        graph.set_route_fn(tag_node, |payload| match payload {
            CoreFlowPayload::Gmem(req) if req.l1_hit => 0,
            _ => 1,
        });

        graph.connect(
            mshr_node,
            wb_node,
            format!("cluster{cluster_id}_l1_mshr_{bank}->l1_wb"),
            link(links.l1_mshr_to_l1_writeback),
        );

        for l2_bank in 0..l2_banks {
            let l2_tag = l2_tag_nodes[l2_bank];
            graph.connect(
                mshr_node,
                l2_tag,
                format!("cluster{cluster_id}_l1_mshr_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_mshr_to_l2_tag),
            );
            graph.connect(
                wb_node,
                l2_tag,
                format!("cluster{cluster_id}_l1_wb_{bank}->l2_tag_{l2_bank}"),
                link(links.l1_writeback_to_l2_tag),
            );
        }
        graph.set_route_fn(mshr_node, move |payload| match payload {
            CoreFlowPayload::Gmem(req) if req.l1_writeback => 0,
            CoreFlowPayload::Gmem(req) => 1 + req.l2_bank,
            _ => 0,
        });
        graph.set_route_fn(wb_node, |payload| match payload {
            CoreFlowPayload::Gmem(req) => req.l2_bank,
            _ => 0,
        });
    }

    let base_core = cluster_id.saturating_mul(cores_per_cluster);
    let max_core = cores_per_cluster.saturating_sub(1);
    let l1_banks = l1_banks.max(1);
    graph.set_route_fn(l1_flush_gate, move |payload| match payload {
        CoreFlowPayload::Gmem(req) if req.kind.is_mem() => req.l1_bank,
        CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1() => {
            let local = req.core_id.saturating_sub(base_core);
            let local = if local >= cores_per_cluster {
                max_core
            } else {
                local
            };
            l1_banks + local
        }
        _ => 0,
    });

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
    let l1_banks = l1_banks.max(1);
    for l2_bank in 0..l2_banks {
        let l2_data = l2_data_nodes[l2_bank];
        let l2_refill = l2_refill_nodes[l2_bank];
        for cluster_id in 0..num_clusters {
            let l1_refill_nodes = &cluster_l1[cluster_id].l1_refill_nodes;
            for l1_bank in 0..l1_banks {
                let l1_refill = l1_refill_nodes[l1_bank];
                graph.connect(
                    l2_data,
                    l1_refill,
                    format!("l2_hit_{l2_bank}->cluster{cluster_id}_l1_refill_{l1_bank}"),
                    link(links.l2_hit_to_l1_refill),
                );
                graph.connect(
                    l2_refill,
                    l1_refill,
                    format!("l2_refill_{l2_bank}->cluster{cluster_id}_l1_refill_{l1_bank}"),
                    link(links.l2_refill_to_l1_refill),
                );
            }
        }

        let l1_banks = l1_banks;
        let num_clusters = num_clusters;
        graph.set_route_fn(l2_data, move |payload| match payload {
            CoreFlowPayload::Gmem(req) => {
                if num_clusters == 0 {
                    return 0;
                }
                let cluster = req.cluster_id.min(num_clusters.saturating_sub(1));
                let bank = req.l1_bank.min(l1_banks.saturating_sub(1));
                cluster * l1_banks + bank
            }
            _ => 0,
        });

        let l1_banks = l1_banks;
        let num_clusters = num_clusters;
        graph.set_route_fn(l2_refill, move |payload| match payload {
            CoreFlowPayload::Gmem(req) => {
                if num_clusters == 0 {
                    return 0;
                }
                let cluster = req.cluster_id.min(num_clusters.saturating_sub(1));
                let bank = req.l1_bank.min(l1_banks.saturating_sub(1));
                cluster * l1_banks + bank
            }
            _ => 0,
        });
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
        let mut cluster_returns = Vec::with_capacity(cores_per_cluster);

        for local_core in 0..cores_per_cluster {
            let ingress_node = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_coalescer"),
                TimedServer::new(nodes.coalescer),
            ));
            let l0_flush_gate = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0_flush_gate"),
                TimedServer::new(nodes.l0_flush_gate),
            ));
            let l0_level = l0_level.expect("gmem.levels[0] (l0) missing");
            let l0_tag = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_tag"),
                TimedServer::new(l0_level.tag),
            ));
            let l0_data = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_data"),
                TimedServer::new(l0_level.data),
            ));
            let l0_mshr = graph.add_node(ServerNode::new(
                format!("cluster{cluster_id}_core{local_core}_l0d_mshr"),
                TimedServer::new(l0_level.mshr),
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

                graph.connect(
                    l0_flush_gate,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->return"),
                    link(links.l0_flush_to_return),
                );
                graph.connect(
                    l0_flush_gate,
                    cluster_state.l1_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->l1_flush"),
                    link(links.l0_flush_to_l1_flush),
                );
                graph.connect(
                    l0_flush_gate,
                    l0_tag,
                    format!("cluster{cluster_id}_core{local_core}_l0_flush->l0_tag"),
                    link(links.l0_flush_to_l0_tag),
                );
                graph.set_route_fn(l0_flush_gate, |payload| match payload {
                    CoreFlowPayload::Gmem(req) if req.kind.is_flush_l0() => 0,
                    CoreFlowPayload::Gmem(req) if req.kind.is_flush_l1() => 1,
                    _ => 2,
                });

                graph.connect(
                    l0_tag,
                    l0_data,
                    format!("cluster{cluster_id}_core{local_core}_l0_tag->l0_hit"),
                    link(links.l0_tag_to_l0_hit),
                );
                graph.connect(
                    l0_tag,
                    l0_mshr,
                    format!("cluster{cluster_id}_core{local_core}_l0_tag->l0_mshr"),
                    link(links.l0_tag_to_l0_mshr),
                );
                graph.set_route_fn(l0_tag, |payload| match payload {
                    CoreFlowPayload::Gmem(req) if req.l0_hit => 0,
                    _ => 1,
                });

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
                graph.connect(
                    ingress_node,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_coalescer->return_flush_l0"),
                    link(links.coalescer_to_l0_flush),
                );
                graph.connect(
                    ingress_node,
                    cluster_state.l1_flush_gate,
                    format!("cluster{cluster_id}_core{local_core}_coalescer->l1_flush"),
                    link(links.coalescer_to_l0_flush),
                );
                graph.set_route_fn(ingress_node, |payload| match payload {
                    CoreFlowPayload::Gmem(req) if req.kind.is_flush_l0() => 0,
                    _ => 1,
                });
            }

            graph.connect(
                cluster_state.l1_flush_gate,
                return_node,
                format!("cluster{cluster_id}_core{local_core}_l1_flush->return"),
                link(links.l1_flush_to_return),
            );

            cluster_returns.push(return_node);
            core_nodes.push(ClusterCoreNodes {
                ingress_node,
                return_node,
            });
        }

        for bank in 0..l1_banks {
            let l1_data = cluster_state.l1_data_nodes[bank];
            let l1_refill = cluster_state.l1_refill_nodes[bank];
            for (local_core, &return_node) in cluster_returns.iter().enumerate() {
                graph.connect(
                    l1_data,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l1_hit_{bank}->return"),
                    link(links.l1_hit_to_return),
                );
                graph.connect(
                    l1_refill,
                    return_node,
                    format!("cluster{cluster_id}_core{local_core}_l1_refill_{bank}->return"),
                    link(links.l1_refill_to_return),
                );
            }

            let base_core = base_core;
            let cores_per_cluster = cores_per_cluster;
            graph.set_route_fn(l1_data, move |payload| match payload {
                CoreFlowPayload::Gmem(req) => {
                    let local = req.core_id.saturating_sub(base_core);
                    if local >= cores_per_cluster {
                        0
                    } else {
                        local
                    }
                }
                _ => 0,
            });

            let base_core = base_core;
            let cores_per_cluster = cores_per_cluster;
            graph.set_route_fn(l1_refill, move |payload| match payload {
                CoreFlowPayload::Gmem(req) => {
                    let local = req.core_id.saturating_sub(base_core);
                    if local >= cores_per_cluster {
                        0
                    } else {
                        local
                    }
                }
                _ => 0,
            });
        }
    }

    core_nodes
}

pub(crate) fn build_cluster_graph(
    config: &GmemFlowConfig,
    num_clusters: usize,
    cores_per_cluster: usize,
) -> (FlowGraph<CoreFlowPayload>, Vec<ClusterCoreNodes>) {
    let mut graph = FlowGraph::new();
    let nodes = &config.nodes;
    let links = &config.links;
    let levels = &config.levels;
    assert!(
        levels.len() >= 3,
        "gmem.levels must define l0/l1/l2 entries"
    );
    let l0_level = Some(&levels[0]);
    let l1_level = &levels[1];
    let l2_level = &levels[2];
    let l1_banks = l1_level.banks.max(1);
    let l2_banks = l2_level.banks.max(1);
    let (l2_tag_nodes, l2_data_nodes, _l2_mshr_nodes, l2_refill_nodes, _l2_wb_nodes, _dram) =
        build_cluster_l2(&mut graph, nodes, links, l2_level);

    let mut cluster_l1 = Vec::with_capacity(num_clusters);
    for cluster_id in 0..num_clusters {
        cluster_l1.push(build_cluster_l1(
            &mut graph,
            nodes,
            links,
            l1_level,
            l2_banks,
            cores_per_cluster,
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
