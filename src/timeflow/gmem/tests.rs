use super::*;
use crate::timeq::Cycle;

const MAX_CYCLES: u64 = 200;

fn make_load(addr: u64, cluster_id: usize) -> GmemRequest {
    let mut req = GmemRequest::new(0, 16, 0xF, true);
    req.addr = addr;
    req.cluster_id = cluster_id;
    req
}

fn issue_flush_l0(cluster: &mut ClusterGmemGraph, core_id: usize, cycle: Cycle, cluster_id: usize) {
    let mut flush = GmemRequest::new_flush_l0(core_id, 1);
    flush.cluster_id = cluster_id;
    cluster
        .issue(core_id, cycle, flush)
        .expect("flush l0 should accept");
}

fn issue_flush_l1(cluster: &mut ClusterGmemGraph, core_id: usize, cycle: Cycle, cluster_id: usize) {
    let mut flush = GmemRequest::new_flush_l1(core_id, 1);
    flush.cluster_id = cluster_id;
    cluster
        .issue(core_id, cycle, flush)
        .expect("flush l1 should accept");
}

fn drain_flushes(cluster: &mut ClusterGmemGraph, core_id: usize, start: Cycle) {
    let mut got_l0 = false;
    let mut got_l1 = false;
    let mut cycle = start;
    for _ in 0..MAX_CYCLES {
        cluster.tick(cycle);
        while let Some(comp) = cluster.pop_completion(core_id) {
            if comp.request.kind.is_flush_l0() {
                got_l0 = true;
            }
            if comp.request.kind.is_flush_l1() {
                got_l1 = true;
            }
        }
        if got_l0 && got_l1 {
            return;
        }
        cycle = cycle.saturating_add(1);
    }
    panic!("expected both flush completions within {MAX_CYCLES} cycles");
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

macro_rules! assert_completes {
    ($cluster:expr, $core_id:expr, $start:expr, $max:expr) => {{
        complete_one($cluster, $core_id, $start, $max)
    }};
}

#[test]
fn cross_core_l2_merge_accepts_second_request() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.levels[2].mshr.queue_capacity = 1;
    let mut cluster = ClusterGmemGraph::new(cfg, 2, 1);
    let now = 0;

    let req0 = make_load(0x1000, 0);
    let req1 = make_load(0x1000, 1);
    let issue0 = cluster.issue(0, now, req0).expect("first request accepts");
    let issue1 = cluster
        .issue(1, now, req1)
        .expect("second request merges at L2");
    assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

    let comp0 = assert_completes!(&mut cluster, 0, now, MAX_CYCLES);
    let comp1 = assert_completes!(&mut cluster, 1, now, MAX_CYCLES);
    assert_eq!(comp0.completed_at, comp1.completed_at);
}

#[test]
fn cross_core_l1_merge_accepts_second_request() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.policy.l0_enabled = true;
    cfg.levels[1].mshr.queue_capacity = 1;
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 2);
    let cycle = 0;

    let req0 = make_load(0x2000, 0);
    cluster
        .issue(0, cycle, req0)
        .expect("warmup request should accept");
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    issue_flush_l0(&mut cluster, 0, cycle, 0);
    issue_flush_l1(&mut cluster, 0, cycle, 0);

    drain_flushes(&mut cluster, 0, cycle);

    let req0 = make_load(0x2000, 0);
    let req1 = make_load(0x2000, 0);
    let issue0 = cluster
        .issue(0, cycle, req0)
        .expect("first post-flush load");
    let issue1 = cluster
        .issue(1, cycle, req1)
        .expect("second load merges at L1");
    assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

    let comp0 = assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);
    let comp1 = assert_completes!(&mut cluster, 1, cycle, MAX_CYCLES);
    assert_eq!(comp0.completed_at, comp1.completed_at);
}

#[test]
fn l0_flush_invalidates_only_l0() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.policy.l0_enabled = true;
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let cycle = 0;

    let req0 = make_load(0x3000, 0);
    cluster.issue(0, cycle, req0).unwrap();
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    issue_flush_l0(&mut cluster, 0, cycle, 0);
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    let req1 = make_load(0x3000, 0);
    cluster.issue(0, cycle, req1).unwrap();
    let comp = assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);
    assert!(!comp.request.l0_hit, "expected L0 miss after flush");
    assert!(comp.request.l1_hit, "expected L1 hit after L0 flush");
}

#[test]
fn l0_disabled_bypasses_l0_and_hits_l1() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.policy.l0_enabled = false;
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let cycle = 0;

    let req0 = make_load(0x3000, 0);
    cluster.issue(0, cycle, req0).unwrap();
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    let req1 = make_load(0x3004, 0);
    cluster.issue(0, cycle, req1).unwrap();
    let comp = assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);
    assert!(!comp.request.l0_hit, "expected L0 disabled -> no L0 hits");
    assert!(comp.request.l1_hit, "expected L1 hit when L0 disabled");
}

#[test]
fn l1_flush_invalidates_cluster_l1() {
    let cfg = GmemFlowConfig::zeroed();
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 2);
    let cycle = 0;

    let req0 = make_load(0x4000, 0);
    cluster.issue(0, cycle, req0).unwrap();
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    issue_flush_l1(&mut cluster, 0, cycle, 0);
    assert_completes!(&mut cluster, 0, cycle, MAX_CYCLES);

    let req1 = make_load(0x4000, 0);
    cluster.issue(1, cycle, req1).unwrap();
    let comp = assert_completes!(&mut cluster, 1, cycle, MAX_CYCLES);
    assert!(!comp.request.l1_hit, "expected L1 miss after flush");
    assert!(comp.request.l2_hit, "expected L2 hit after L1 flush");
}

#[test]
fn merge_completion_fanout_same_core() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.levels[2].mshr.queue_capacity = 1;
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let now = 0;

    let req0 = make_load(0x5000, 0);
    let req1 = make_load(0x5000, 0);
    let issue0 = cluster.issue(0, now, req0).unwrap();
    let issue1 = cluster.issue(0, now, req1).unwrap();
    assert_eq!(issue0.ticket.ready_at(), issue1.ticket.ready_at());

    let comp0 = assert_completes!(&mut cluster, 0, now, MAX_CYCLES);
    let comp1 = assert_completes!(&mut cluster, 0, now, MAX_CYCLES);
    assert_eq!(comp0.completed_at, comp1.completed_at);
}

#[test]
fn mshr_full_rejects_with_retry_cycle() {
    let mut cfg = GmemFlowConfig::zeroed();
    cfg.levels[2].mshr.queue_capacity = 1;
    cfg.levels[1].mshr.queue_capacity = 2;
    cfg.levels[0].mshr.queue_capacity = 2;
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let now = 0;

    let req0 = make_load(0x6000, 0);
    cluster.issue(0, now, req0).unwrap();
    let req1 = make_load(0x8000, 0);
    let err = cluster
        .issue(0, now, req1)
        .expect_err("MSHR should be full");
    assert_eq!(GmemRejectReason::QueueFull, err.reason);
    assert!(err.retry_at > now);
}

#[test]
fn address_zero_load_completes() {
    let cfg = GmemFlowConfig::zeroed();
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let mut req = GmemRequest::new(0, 16, 0xF, true);
    req.addr = 0;
    req.cluster_id = 0;
    cluster.issue(0, 0, req).unwrap();
    let _ = assert_completes!(&mut cluster, 0, 0, MAX_CYCLES);
}

#[test]
fn unaligned_address_handling_completes() {
    let cfg = GmemFlowConfig::zeroed();
    let mut cluster = ClusterGmemGraph::new(cfg, 1, 1);
    let mut req = GmemRequest::new(0, 16, 0xF, true);
    req.addr = 0x123;
    req.cluster_id = 0;
    cluster.issue(0, 0, req).unwrap();
    let _ = assert_completes!(&mut cluster, 0, 0, MAX_CYCLES);
}
