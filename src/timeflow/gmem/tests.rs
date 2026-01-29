use super::*;
use crate::timeflow::graph::FlowGraph;
use crate::timeflow::types::CoreFlowPayload;
use crate::timeq::{Cycle, ServerConfig};

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

#[test]
fn address_zero_load_completes() {
    let cfg = fast_config();
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let mut req = GmemRequest::new(0, 16, 0xF, true);
    req.addr = 0;
    req.cluster_id = 0;
    cluster.issue(0, 0, req).unwrap();
    let _ = complete_one(&mut cluster, 0, 0, 200);
}

#[test]
fn unaligned_address_handling_completes() {
    let cfg = fast_config();
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let mut req = GmemRequest::new(0, 16, 0xF, true);
    req.addr = 0x123;
    req.cluster_id = 0;
    cluster.issue(0, 0, req).unwrap();
    let _ = complete_one(&mut cluster, 0, 0, 200);
}
