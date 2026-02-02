use std::collections::VecDeque;

use crate::timeflow::graph::FlowGraph;
use crate::timeflow::types::{CoreFlowPayload, NodeId};
use crate::timeq::{Backpressure, Cycle, ServiceRequest, Ticket};

use super::cache::CacheTagArray;
use super::graph_build::{build_cluster_graph, GmemFlowConfig};
use super::mshr::{AdmissionBackpressure, MissLevel, MissMetadata, MshrAdmission, MshrTable};
use super::policy::{bank_for, decide, line_addr, GmemPolicyConfig};
use super::request::{extract_gmem_request, GmemCompletion, GmemIssue, GmemReject, GmemRejectReason, GmemRequest, GmemResult};
use super::stats::GmemStats;

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
        let nodes = &config.nodes;
        let l1_banks = config.policy.l1_banks.max(1);
        let l2_banks = config.policy.l2_banks.max(1);

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

        let l0_tags = (0..total_cores)
            .map(|_| CacheTagArray::new(l0_sets, l0_ways))
            .collect();
        let l1_tags = (0..num_clusters)
            .map(|_| CacheTagArray::new(l1_sets, l1_ways))
            .collect();

        let l2_tags = CacheTagArray::new(l2_sets, l2_ways);

        let l0_mshr_capacity = nodes.l0d_mshr.queue_capacity;
        let l1_mshr_capacity = nodes.l1_mshr.queue_capacity;
        let l2_mshr_capacity = nodes.l2_mshr.queue_capacity;

        let l0_mshrs = (0..total_cores).map(|_| MshrTable::new(l0_mshr_capacity)).collect();
        let l1_mshrs = (0..num_clusters)
            .map(|_| (0..l1_banks).map(|_| MshrTable::new(l1_mshr_capacity)).collect())
            .collect();
        let l2_mshrs = (0..l2_banks).map(|_| MshrTable::new(l2_mshr_capacity)).collect();

        let l0_mshr_admit = (0..total_cores).map(|_| MshrAdmission::new(nodes.l0d_mshr)).collect();
        let l1_mshr_admit = (0..num_clusters)
            .map(|_| (0..l1_banks).map(|_| MshrAdmission::new(nodes.l1_mshr)).collect())
            .collect();
        let l2_mshr_admit = (0..l2_banks).map(|_| MshrAdmission::new(nodes.l2_mshr)).collect();

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
        if cluster_id >= self.l1_tags.len() {
            let retry_at = now.saturating_add(1);
            return Err(GmemReject {
                payload: request,
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
        request.l1_bank = bank_for(l1_line, l1_banks, L1_BANK_SEED);
        request.l2_bank = bank_for(l2_line, l2_banks, L2_BANK_SEED);

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
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l0_mshr_admit[core_id].try_admit(now) {
                    return Err(self.reject_for_admission(core_id, &request, bp));
                }
                l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
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
                if !self.l0_mshrs[core_id].has_entry(l0_line) {
                    l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
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
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l1_mshr_admit[cluster_id][l1_bank].try_admit(now) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    return Err(self.reject_for_admission(core_id, &request, bp));
                }
                l1_new = match self.l1_mshrs[cluster_id][l1_bank].ensure_entry(l1_line, meta) {
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
                if !self.l0_mshrs[core_id].has_entry(l0_line) {
                    l0_new = match self.l0_mshrs[core_id].ensure_entry(l0_line, meta) {
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

                if !self.l1_mshrs[cluster_id][l1_bank].has_entry(l1_line) {
                    l1_new = match self.l1_mshrs[cluster_id][l1_bank].ensure_entry(l1_line, meta) {
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
                    self.cores[core_id].stats.record_queue_full_reject();
                    return Err(GmemReject {
                        payload: request,
                        retry_at: now.saturating_add(1),
                        reason: GmemRejectReason::QueueFull,
                    });
                }
                if let Err(bp) = self.l2_mshr_admit[l2_bank].try_admit(now) {
                    self.rollback_mshrs(
                        core_id, cluster_id, l1_bank, l2_bank, l0_line, l1_line, l2_line,
                        l0_new, l1_new, l2_new,
                    );
                    return Err(self.reject_for_admission(core_id, &request, bp));
                }
                l2_new = match self.l2_mshrs[l2_bank].ensure_entry(l2_line, meta) {
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
    ) -> GmemResult<GmemIssue> {
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

    fn reject_for_admission(
        &mut self,
        core_id: usize,
        request: &GmemRequest,
        bp: AdmissionBackpressure,
    ) -> GmemReject {
        let stats = &mut self.cores[core_id].stats;
        match bp {
            AdmissionBackpressure::Busy { retry_at } => {
                stats.record_busy_reject();
                GmemReject {
                    payload: request.clone(),
                    retry_at,
                    reason: GmemRejectReason::Busy,
                }
            }
            AdmissionBackpressure::QueueFull { retry_at } => {
                stats.record_queue_full_reject();
                GmemReject {
                    payload: request.clone(),
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
        if !request.is_load {
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
                .map(|entry| entry.merged.into_vec())
                .unwrap_or_default(),
            MissLevel::L1 => {
                let cluster_id = request.cluster_id;
                let l1_bank = request.l1_bank;
                let merged = if cluster_id < self.l1_mshrs.len()
                    && l1_bank < self.l1_mshrs[cluster_id].len()
                {
                    self.l1_mshrs[cluster_id][l1_bank]
                        .remove_entry(l1_line)
                        .map(|entry| entry.merged.into_vec())
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
                        .map(|entry| entry.merged.into_vec())
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
