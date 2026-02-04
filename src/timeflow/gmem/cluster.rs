use std::collections::VecDeque;

use crate::timeflow::graph::FlowGraph;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServiceRequest, Ticket};

use super::cache::CacheTagArray;
use super::graph_build::{build_cluster_graph, GmemFlowConfig};
use super::mshr::{MissLevel, MissMetadata, MshrTable};
use super::policy::{bank_for, decide, line_addr, GmemPolicyConfig};
use super::request::{
    extract_gmem_request, GmemCompletion, GmemIssue, GmemReject, GmemRejectReason, GmemRequest,
    GmemResult,
};
use super::stats::GmemStats;

/// A unified Bank type that pairs an MSHR table, admission controller, and
/// per-bank statistics. This will be used by the layered topology `CacheLayer`.
pub struct Bank {
    pub mshr: MshrTable,
    pub stats: GmemStats,
}

impl Bank {
    pub fn new(mshr_capacity: usize) -> Self {
        Self {
            mshr: MshrTable::new(mshr_capacity),
            stats: GmemStats::default(),
        }
    }
}

/// A CacheLayer owns a `CacheTagArray` and one or more `Bank`s. This models the
/// hardware concept of a cache layer (tags + data/MSHR banks) and centralizes
/// tag + bank access logic.
pub struct CacheLayer {
    pub tags: CacheTagArray,
    pub banks: Vec<Bank>,
}

impl CacheLayer {
    pub fn new(tags: CacheTagArray, num_banks: usize, mshr_capacity: usize) -> Self {
        let banks = (0..num_banks).map(|_| Bank::new(mshr_capacity)).collect();
        Self { tags, banks }
    }

    pub fn bank_count(&self) -> usize {
        self.banks.len()
    }
}

/// A hierarchical container for the layered cache topology.
pub struct GmemHierarchy {
    pub l0: Vec<CacheLayer>,
    pub l1: Vec<CacheLayer>,
    pub l2: CacheLayer,
}

impl GmemHierarchy {
    pub fn new(l0: Vec<CacheLayer>, l1: Vec<CacheLayer>, l2: CacheLayer) -> Self {
        Self { l0, l1, l2 }
    }

    /// Aggregate stats across all layers into a single `GmemStats`.
    pub fn aggregate_stats(&self) -> GmemStats {
        let mut s = GmemStats::default();
        for layer in &self.l0 {
            s.accumulate_from(&layer.stats());
        }
        for layer in &self.l1 {
            s.accumulate_from(&layer.stats());
        }
        s.accumulate_from(&self.l2.stats());
        s
    }

    /// Return per-level aggregated stats: (l0_all, l1_all, l2)
    pub fn per_level_stats(&self) -> (GmemStats, GmemStats, GmemStats) {
        let mut l0 = GmemStats::default();
        for layer in &self.l0 {
            l0.accumulate_from(&layer.stats());
        }
        let mut l1 = GmemStats::default();
        for layer in &self.l1 {
            l1.accumulate_from(&layer.stats());
        }
        let l2 = self.l2.stats();
        (l0, l1, l2)
    }
}

impl CacheLayer {
    pub fn probe(&mut self, line: u64) -> bool {
        self.tags.probe(line)
    }

    pub fn has_entry(&self, bank_idx: usize, line: u64) -> bool {
        if bank_idx >= self.bank_count() {
            return false;
        }
        self.banks[bank_idx].mshr.has_entry(line)
    }

    pub fn merge_request(&mut self, bank_idx: usize, line: u64, req: GmemRequest) -> Option<Cycle> {
        if bank_idx >= self.bank_count() {
            return None;
        }
        self.banks[bank_idx].mshr.merge_request(line, req)
    }

    pub fn can_allocate(&self, bank_idx: usize, line: u64) -> bool {
        if bank_idx >= self.bank_count() {
            return false;
        }
        self.banks[bank_idx].mshr.can_allocate(line)
    }

    pub fn ensure_entry(
        &mut self,
        bank_idx: usize,
        line: u64,
        meta: MissMetadata,
    ) -> Result<bool, ()> {
        if bank_idx >= self.bank_count() {
            return Err(());
        }
        match self.banks[bank_idx].mshr.ensure_entry(line, meta) {
            Ok(v) => Ok(v),
            Err(_) => Err(()),
        }
    }

    pub fn set_ready_at(&mut self, bank_idx: usize, line: u64, at: Cycle) {
        if bank_idx >= self.bank_count() {
            return;
        }
        self.banks[bank_idx].mshr.set_ready_at(line, at);
    }

    pub fn remove_entry_merged(&mut self, bank_idx: usize, line: u64) -> Vec<GmemRequest> {
        if bank_idx >= self.bank_count() {
            return Vec::new();
        }
        self.banks[bank_idx]
            .mshr
            .remove_entry(line)
            .map(|entry| entry.merged)
            .unwrap_or_default()
    }

    /// Aggregate stats for this cache layer by summing per-bank stats.
    pub fn stats(&self) -> GmemStats {
        let mut s = GmemStats::default();
        for b in &self.banks {
            s.accumulate_from(&b.stats);
        }
        s
    }
}

pub struct ClusterGmemGraph {
    graph: FlowGraph<CoreFlowPayload>,
    cores: Vec<ClusterCoreState>,
    policy: GmemPolicyConfig,
    // Tags and bank groups replaced by `hierarchy`.
    l1_banks: usize,
    hierarchy: GmemHierarchy,
    last_tick: Cycle,
}

const L1_BANK_SEED: u64 = 0x1111_2222_3333_4444;
const L2_BANK_SEED: u64 = 0x5555_6666_7777_8888;

struct ClusterCoreState {
    ingress_node: NodeId,
    return_node: NodeId,
    completions: VecDeque<GmemCompletion>,
    stats: GmemStats,
    next_id: u64,
}

impl ClusterGmemGraph {
    // helper functions were simplified to iterator-based initializers.

    pub fn new(config: GmemFlowConfig, num_clusters: usize, cores_per_cluster: usize) -> Self {
        let (graph, core_nodes) = build_cluster_graph(&config, num_clusters, cores_per_cluster);
        let levels = &config.levels;
        assert!(
            levels.len() >= 3,
            "gmem.levels must define l0/l1/l2 entries"
        );
        let l0_level = &levels[0];
        let l1_level = &levels[1];
        let l2_level = &levels[2];
        let l1_banks = l1_level.banks.max(1);
        let l2_banks = l2_level.banks.max(1);

        let total_cores = num_clusters.saturating_mul(cores_per_cluster);
        let mut cores = Vec::with_capacity(total_cores);
        for node in core_nodes {
            cores.push(ClusterCoreState {
                ingress_node: node.ingress_node,
                return_node: node.return_node,
                completions: VecDeque::new(),
                stats: GmemStats::default(),
                next_id: 0,
            });
        }

        let policy = config.policy;
        let l0_sets = policy.l0_sets.max(1);
        let l0_ways = policy.l0_ways.max(1);
        let l1_sets = policy.l1_sets.max(1);
        let l1_ways = policy.l1_ways.max(1);
        let l2_sets = policy.l2_sets.max(1);
        let l2_ways = policy.l2_ways.max(1);

        let l0_mshr_capacity = l0_level.mshr.queue_capacity;
        let l1_mshr_capacity = l1_level.mshr.queue_capacity;
        let l2_mshr_capacity = l2_level.mshr.queue_capacity;

        // Build the hierarchical cache topology (fresh tag arrays for now).
        let l0_layers = if policy.l0_enabled {
            (0..total_cores)
                .map(|_| CacheLayer::new(CacheTagArray::new(l0_sets, l0_ways), 1, l0_mshr_capacity))
                .collect()
        } else {
            Vec::new()
        };
        let l1_layers = (0..num_clusters)
            .map(|_| {
                CacheLayer::new(
                    CacheTagArray::new(l1_sets, l1_ways),
                    l1_banks,
                    l1_mshr_capacity,
                )
            })
            .collect();
        let l2_layer = CacheLayer::new(
            CacheTagArray::new(l2_sets, l2_ways),
            l2_banks,
            l2_mshr_capacity,
        );
        let hierarchy = GmemHierarchy::new(l0_layers, l1_layers, l2_layer);

        Self {
            graph,
            cores,
            policy,
            l1_banks,
            hierarchy,
            last_tick: u64::MAX,
        }
    }

    pub fn issue(
        &mut self,
        core_id: usize,
        now: Cycle,
        mut request: GmemRequest,
    ) -> GmemResult<GmemIssue> {
        if core_id >= self.cores.len() {
            return Err(GmemReject {
                payload: request,
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
        if cluster_id >= self.hierarchy.l1.len() {
            let retry_at = now.saturating_add(1);
            return Err(GmemReject {
                payload: request,
                retry_at,
                reason: GmemRejectReason::QueueFull,
            });
        }

        let policy = self.policy;
        let l0_enabled = policy.l0_enabled;
        let l0_line = if l0_enabled {
            line_addr(request.addr, policy.l0_line_bytes)
        } else {
            0
        };
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        request.line_addr = l2_line;
        let l1_banks = self.l1_banks.max(1) as u64;
        let l2_banks = self.hierarchy.l2.bank_count().max(1) as u64;
        request.l1_bank = bank_for(l1_line, l1_banks, L1_BANK_SEED);
        request.l2_bank = bank_for(l2_line, l2_banks, L2_BANK_SEED);

        let l1_bank = request.l1_bank;
        let l2_bank = request.l2_bank;

        let l0_hit = if l0_enabled && core_id < self.hierarchy.l0.len() {
            let hit = self.hierarchy.l0[core_id].probe(l0_line);
            if self.hierarchy.l0[core_id].bank_count() > 0 {
                let bytes = request.bytes;
                self.hierarchy.l0[core_id].banks[0]
                    .stats
                    .record_access(bytes);
                if hit {
                    self.hierarchy.l0[core_id].banks[0].stats.record_hit(bytes);
                }
            }
            hit
        } else {
            false
        };
        request.l0_hit = l0_hit;
        if l0_hit {
            request.l1_hit = false;
            request.l2_hit = false;
            request.l1_writeback = false;
            request.l2_writeback = false;
        } else {
            let l1_hit = { self.hierarchy.l1[cluster_id].probe(l1_line) };
            request.l1_hit = l1_hit;
            // Record L1 access/hit on selected bank if present
            if cluster_id < self.hierarchy.l1.len() {
                let l1_layer = &mut self.hierarchy.l1[cluster_id];
                if (l1_layer.bank_count() > 0) && ((l1_bank as usize) < l1_layer.bank_count()) {
                    let bytes = request.bytes;
                    l1_layer.banks[l1_bank as usize].stats.record_access(bytes);
                    if l1_hit {
                        l1_layer.banks[l1_bank as usize].stats.record_hit(bytes);
                    }
                }
            }
            if l1_hit {
                request.l2_hit = false;
            } else {
                request.l2_hit = self.hierarchy.l2.probe(l2_line);
                // Record L2 access/hit on selected bank
                if (self.hierarchy.l2.bank_count() > 0)
                    && ((l2_bank as usize) < self.hierarchy.l2.bank_count())
                {
                    let bytes = request.bytes;
                    self.hierarchy.l2.banks[l2_bank as usize]
                        .stats
                        .record_access(bytes);
                    if request.l2_hit {
                        self.hierarchy.l2.banks[l2_bank as usize]
                            .stats
                            .record_hit(bytes);
                    }
                }
            }

            if !request.l1_hit {
                let l1_key =
                    l1_line ^ (cluster_id as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f) ^ policy.seed;
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

        let miss_level = if l0_enabled {
            if request.l0_hit {
                MissLevel::None
            } else if request.l1_hit {
                MissLevel::L0
            } else if request.l2_hit {
                MissLevel::L1
            } else {
                MissLevel::L2
            }
        } else if request.l1_hit {
            MissLevel::None
        } else if request.l2_hit {
            MissLevel::L1
        } else {
            MissLevel::L2
        };

        let meta = MissMetadata::from_request(&request);
        let mut l0_new = false;
        let mut l1_new = false;
        let mut l2_new = false;

        match miss_level {
            MissLevel::None => {}
            MissLevel::L0 => {
                if self.hierarchy.l0[core_id].has_entry(0, l0_line) {
                    let bytes = request.bytes;
                    let ready_at = self.hierarchy.l0[core_id]
                        .merge_request(0, l0_line, request)
                        .unwrap_or(now.saturating_add(1));
                    return Ok(self.issue_merge(core_id, assigned_id, now, ready_at, bytes));
                }

                if !self.hierarchy.l0[core_id].can_allocate(0, l0_line) {
                    // attribute queue-full reject to L0 bank
                    if core_id < self.hierarchy.l0.len()
                        && self.hierarchy.l0[core_id].bank_count() > 0
                    {
                        self.hierarchy.l0[core_id].banks[0]
                            .stats
                            .record_queue_full_reject();
                    }
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }

                l0_new = match self.hierarchy.l0[core_id].ensure_entry(0, l0_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.cores[core_id].stats.record_queue_full_reject();
                        return Err(GmemReject {
                            payload: request,
                            retry_at: now.saturating_add(1),
                            reason: GmemRejectReason::QueueFull,
                        });
                    }
                };
            }
            MissLevel::L1 => {
                if l0_enabled {
                    if !self.hierarchy.l0[core_id].has_entry(0, l0_line) {
                        l0_new = match self.hierarchy.l0[core_id].ensure_entry(0, l0_line, meta) {
                            Ok(new_entry) => new_entry,
                            Err(_) => {
                                self.cores[core_id].stats.record_queue_full_reject();
                                return Err(GmemReject {
                                    payload: request,
                                    retry_at: now.saturating_add(1),
                                    reason: GmemRejectReason::QueueFull,
                                });
                            }
                        };
                    }
                }

                if self.hierarchy.l1[cluster_id].has_entry(l1_bank, l1_line) {
                    let bytes = request.bytes;
                    let ready_at = self.hierarchy.l1[cluster_id]
                        .merge_request(l1_bank, l1_line, request)
                        .unwrap_or(now.saturating_add(1));
                    if l0_enabled {
                        self.hierarchy.l0[core_id].set_ready_at(0, l0_line, ready_at);
                    }
                    return Ok(self.issue_merge(core_id, assigned_id, now, ready_at, bytes));
                }

                if !self.hierarchy.l1[cluster_id].can_allocate(l1_bank, l1_line) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line, l0_new,
                        l1_new, l2_new,
                    );
                    if cluster_id < self.hierarchy.l1.len()
                        && (l1_bank as usize) < self.hierarchy.l1[cluster_id].bank_count()
                    {
                        self.hierarchy.l1[cluster_id].banks[l1_bank as usize]
                            .stats
                            .record_queue_full_reject();
                    }
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }

                l1_new = match self.hierarchy.l1[cluster_id].ensure_entry(l1_bank, l1_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.rollback_mshrs(
                            core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                            l0_new, l1_new, l2_new,
                        );
                        self.cores[core_id].stats.record_queue_full_reject();
                        return Err(GmemReject {
                            payload: request,
                            retry_at: now.saturating_add(1),
                            reason: GmemRejectReason::QueueFull,
                        });
                    }
                };
            }
            MissLevel::L2 => {
                if l0_enabled {
                    if !self.hierarchy.l0[core_id].has_entry(0, l0_line) {
                        l0_new = match self.hierarchy.l0[core_id].ensure_entry(0, l0_line, meta) {
                            Ok(new_entry) => new_entry,
                            Err(_) => {
                                self.cores[core_id].stats.record_queue_full_reject();
                                return Err(GmemReject {
                                    payload: request,
                                    retry_at: now.saturating_add(1),
                                    reason: GmemRejectReason::QueueFull,
                                });
                            }
                        };
                    }
                }

                if !self.hierarchy.l1[cluster_id].has_entry(l1_bank, l1_line) {
                    l1_new =
                        match self.hierarchy.l1[cluster_id].ensure_entry(l1_bank, l1_line, meta) {
                            Ok(new_entry) => new_entry,
                            Err(_) => {
                                self.rollback_mshrs(
                                    core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line,
                                    l2_line, l0_new, l1_new, l2_new,
                                );
                                self.cores[core_id].stats.record_queue_full_reject();
                                return Err(GmemReject {
                                    payload: request,
                                    retry_at: now.saturating_add(1),
                                    reason: GmemRejectReason::QueueFull,
                                });
                            }
                        };
                }

                if self.hierarchy.l2.has_entry(l2_bank, l2_line) {
                    let bytes = request.bytes;
                    let ready_at = self
                        .hierarchy
                        .l2
                        .merge_request(l2_bank, l2_line, request)
                        .unwrap_or(now.saturating_add(1));
                    self.hierarchy.l1[cluster_id].set_ready_at(l1_bank, l1_line, ready_at);
                    if l0_enabled {
                        self.hierarchy.l0[core_id].set_ready_at(0, l0_line, ready_at);
                    }
                    return Ok(self.issue_merge(core_id, assigned_id, now, ready_at, bytes));
                }

                if !self.hierarchy.l2.can_allocate(l2_bank, l2_line) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line, l0_new,
                        l1_new, l2_new,
                    );
                    if (l2_bank as usize) < self.hierarchy.l2.bank_count() {
                        self.hierarchy.l2.banks[l2_bank as usize]
                            .stats
                            .record_queue_full_reject();
                    }
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }

                l2_new = match self.hierarchy.l2.ensure_entry(l2_bank, l2_line, meta) {
                    Ok(new_entry) => new_entry,
                    Err(_) => {
                        self.rollback_mshrs(
                            core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                            l0_new, l1_new, l2_new,
                        );
                        self.cores[core_id].stats.record_queue_full_reject();
                        return Err(GmemReject {
                            payload: request,
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
                if l0_enabled {
                    self.hierarchy.l0[core_id].set_ready_at(0, l0_line, ready_at);
                }
            }
            MissLevel::L1 => {
                if l0_enabled {
                    self.hierarchy.l0[core_id].set_ready_at(0, l0_line, ready_at);
                }
                self.hierarchy.l1[cluster_id].set_ready_at(l1_bank, l1_line, ready_at);
            }
            MissLevel::L2 => {
                if l0_enabled {
                    self.hierarchy.l0[core_id].set_ready_at(0, l0_line, ready_at);
                }
                self.hierarchy.l1[cluster_id].set_ready_at(l1_bank, l1_line, ready_at);
                self.hierarchy.l2.set_ready_at(l2_bank, l2_line, ready_at);
            }
        }

        Ok(issue)
    }

    pub fn tick(&mut self, now: Cycle) {
        if now == self.last_tick {
            return;
        }
        self.last_tick = now;

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

                let merged = self.drain_mshr_merges(&request, now);
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
    ) -> GmemResult<GmemIssue> {
        let request_id = request.id;
        let bytes = request.bytes;
        let payload = CoreFlowPayload::Gmem(request);
        let service_req = ServiceRequest::new(payload, bytes);

        match self
            .graph
            .try_put(self.cores[core_id].ingress_node, now, service_req)
        {
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
                    stats.record_busy_reject();
                    let mut retry_at = available_at;
                    if retry_at <= now {
                        retry_at = now.saturating_add(1);
                    }
                    let request = extract_gmem_request(request);
                    Err(GmemReject {
                        payload: request,
                        retry_at,
                        reason: GmemRejectReason::Busy,
                    })
                }
                Backpressure::QueueFull { request, .. } => {
                    let stats = &mut self.cores[core_id].stats;
                    stats.record_queue_full_reject();
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
            core_state.stats.record_issue(bytes);
        }
    }

    fn push_completion(&mut self, request: GmemRequest, ticket_ready_at: Cycle, now: Cycle) {
        let core_id = request.core_id;
        if let Some(core_state) = self.cores.get_mut(core_id) {
            core_state.stats.record_completion(request.bytes, now);
            core_state.completions.push_back(GmemCompletion {
                ticket_ready_at,
                completed_at: now,
                request,
            });
            core_state
                .stats
                .update_completion_queue(core_state.completions.len());
        }
    }

    fn apply_completion_effects(&mut self, request: &GmemRequest) {
        if request.kind.is_flush_l0() {
            if self.policy.l0_enabled && request.core_id < self.hierarchy.l0.len() {
                self.hierarchy.l0[request.core_id].tags.invalidate_all();
            }
            return;
        }
        if request.kind.is_flush_l1() {
            if request.cluster_id < self.hierarchy.l1.len() {
                self.hierarchy.l1[request.cluster_id].tags.invalidate_all();
            }
            return;
        }
        if !request.kind.is_mem() {
            return;
        }
        if !request.is_load {
            return;
        }

        let policy = self.policy;
        let l0_enabled = policy.l0_enabled;
        let l0_line = if l0_enabled {
            line_addr(request.addr, policy.l0_line_bytes)
        } else {
            0
        };
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        if l0_enabled && !request.l0_hit && request.core_id < self.hierarchy.l0.len() {
            self.hierarchy.l0[request.core_id].tags.fill(l0_line);
        }
        if !request.l1_hit && request.cluster_id < self.hierarchy.l1.len() {
            self.hierarchy.l1[request.cluster_id].tags.fill(l1_line);
        }
        if !request.l2_hit {
            self.hierarchy.l2.tags.fill(l2_line);
        }
    }

    fn drain_mshr_merges(&mut self, request: &GmemRequest, now: Cycle) -> Vec<GmemRequest> {
        if !request.kind.is_mem() {
            return Vec::new();
        }

        let miss_level = if self.policy.l0_enabled {
            if request.l0_hit {
                MissLevel::None
            } else if request.l1_hit {
                MissLevel::L0
            } else if request.l2_hit {
                MissLevel::L1
            } else {
                MissLevel::L2
            }
        } else if request.l1_hit {
            MissLevel::None
        } else if request.l2_hit {
            MissLevel::L1
        } else {
            MissLevel::L2
        };

        if miss_level == MissLevel::None {
            return Vec::new();
        }

        let policy = self.policy;
        let l0_enabled = policy.l0_enabled;
        let l0_line = if l0_enabled {
            line_addr(request.addr, policy.l0_line_bytes)
        } else {
            0
        };
        let l1_line = line_addr(request.addr, policy.l1_line_bytes);
        let l2_line = line_addr(request.addr, policy.l2_line_bytes);

        match miss_level {
            MissLevel::L0 => {
                if l0_enabled {
                    self.hierarchy.l0[request.core_id].remove_entry_merged(0, l0_line)
                } else {
                    Vec::new()
                }
            }
            MissLevel::L1 => {
                let cluster_id = request.cluster_id;
                let l1_bank = request.l1_bank;
                if cluster_id < self.hierarchy.l1.len()
                    && (l1_bank as usize) < self.hierarchy.l1[cluster_id].bank_count()
                {
                    let merged =
                        self.hierarchy.l1[cluster_id].remove_entry_merged(l1_bank, l1_line);
                    // attribute completions to the L1 bank (primary + merged)
                    for req in std::iter::once(request.clone()).chain(merged.iter().cloned()) {
                        if (l1_bank as usize) < self.hierarchy.l1[cluster_id].bank_count() {
                            self.hierarchy.l1[cluster_id].banks[l1_bank as usize]
                                .stats
                                .record_completion(req.bytes, now);
                        }
                    }
                    // Remove corresponding L0 placeholders
                    if l0_enabled {
                        for req in std::iter::once(request.clone()).chain(merged.iter().cloned()) {
                            if req.core_id < self.hierarchy.l0.len() {
                                let l0_line_req = line_addr(req.addr, policy.l0_line_bytes);
                                let _ = self.hierarchy.l0[req.core_id]
                                    .remove_entry_merged(0, l0_line_req);
                            }
                        }
                    }
                    merged
                } else {
                    Vec::new()
                }
            }
            MissLevel::L2 => {
                let l2_bank = request.l2_bank;
                if (l2_bank as usize) < self.hierarchy.l2.bank_count() {
                    let merged = self.hierarchy.l2.remove_entry_merged(l2_bank, l2_line);
                    // attribute completions to the L2 bank (primary + merged)
                    for req in std::iter::once(request.clone()).chain(merged.iter().cloned()) {
                        if (l2_bank as usize) < self.hierarchy.l2.bank_count() {
                            self.hierarchy.l2.banks[l2_bank as usize]
                                .stats
                                .record_completion(req.bytes, now);
                        }
                    }
                    // Remove corresponding placeholders in L1 and L0
                    for req in std::iter::once(request.clone()).chain(merged.iter().cloned()) {
                        if l0_enabled && req.core_id < self.hierarchy.l0.len() {
                            let l0_line_req = line_addr(req.addr, policy.l0_line_bytes);
                            let _ =
                                self.hierarchy.l0[req.core_id].remove_entry_merged(0, l0_line_req);
                        }
                        let cluster_id = req.cluster_id;
                        if cluster_id < self.hierarchy.l1.len() {
                            let l1_line_req = line_addr(req.addr, policy.l1_line_bytes);
                            let l1_bank_req = req.l1_bank.min(self.l1_banks.saturating_sub(1));
                            if (l1_bank_req as usize) < self.hierarchy.l1[cluster_id].bank_count() {
                                let _ = self.hierarchy.l1[cluster_id]
                                    .remove_entry_merged(l1_bank_req as usize, l1_line_req);
                            }
                        }
                    }
                    merged
                } else {
                    Vec::new()
                }
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
        if self.policy.l0_enabled && l0_new && core_id < self.hierarchy.l0.len() {
            let _ = self.hierarchy.l0[core_id].remove_entry_merged(0, l0_line);
        }
        if l1_new
            && cluster_id < self.hierarchy.l1.len()
            && l1_bank < self.hierarchy.l1[cluster_id].bank_count()
        {
            let _ = self.hierarchy.l1[cluster_id].remove_entry_merged(l1_bank, l1_line);
        }
        if l2_new && l2_bank < self.hierarchy.l2.bank_count() {
            let _ = self.hierarchy.l2.remove_entry_merged(l2_bank, l2_line);
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

    /// Aggregate stats across the hierarchical cache (all layers).
    pub fn hierarchy_stats(&self) -> GmemStats {
        self.hierarchy.aggregate_stats()
    }

    /// Return per-level aggregated stats: (l0_all, l1_all, l2)
    pub fn hierarchy_stats_per_level(&self) -> (GmemStats, GmemStats, GmemStats) {
        self.hierarchy.per_level_stats()
    }

    /// Return per-cluster (L1) stats as a vector where index = cluster id.
    pub fn l1_cluster_stats(&self) -> Vec<GmemStats> {
        self.hierarchy
            .l1
            .iter()
            .map(|layer| layer.stats())
            .collect()
    }

    /// Return per-bank stats for L2 as a vector.
    pub fn l2_bank_stats(&self) -> Vec<GmemStats> {
        self.hierarchy.l2.banks.iter().map(|b| b.stats).collect()
    }

    /// Aggregate L2 stats across banks.
    pub fn l2_stats(&self) -> GmemStats {
        self.hierarchy.l2.stats()
    }

    /// Return per-level hit rates (l0, l1, l2) as (0.0..1.0).
    pub fn hierarchy_hit_rates(&self) -> (f64, f64, f64) {
        let (l0, l1, l2) = self.hierarchy.per_level_stats();
        let l0_rate = if l0.accesses() == 0 {
            0.0
        } else {
            l0.hits() as f64 / l0.accesses() as f64
        };
        let l1_rate = if l1.accesses() == 0 {
            0.0
        } else {
            l1.hits() as f64 / l1.accesses() as f64
        };
        let l2_rate = if l2.accesses() == 0 {
            0.0
        } else {
            l2.hits() as f64 / l2.accesses() as f64
        };
        (l0_rate, l1_rate, l2_rate)
    }
}
