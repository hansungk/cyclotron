use crate::timeflow::lsu::{LsuCompletion, LsuFlowConfig, LsuPayload, LsuRejectReason, LsuSubgraph};
use crate::timeflow::{GmemRequest, GmemRequestKind, SmemRequest};
use crate::timeq::{ServerConfig, Cycle};

fn default_issue_config() -> ServerConfig {
    ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 1024,
        queue_capacity: 4,
        completions_per_cycle: 1,
        ..ServerConfig::default()
    }
}

fn drain_issued(lsu: &mut LsuSubgraph, now: Cycle, max_cycles: u64) -> Vec<LsuCompletion<LsuPayload>> {
    let mut result = Vec::new();
    for i in 0..max_cycles {
        let cycle = now.saturating_add(i);
        lsu.tick(cycle);
        while let Some(payload) = lsu.take_ready(cycle) {
            result.push(payload);
        }
    }
    result
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

    let issued = drain_issued(&mut lsu, 0, 10);

    assert!(
        matches!(issued.get(0), Some(completion) if matches!(completion.request, LsuPayload::Smem(_))),
        "expected shared request to issue before global"
    );
    assert!(
        matches!(issued.get(1), Some(completion) if matches!(completion.request, LsuPayload::Gmem(_))),
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

    let issued = drain_issued(&mut lsu, 0, 10);

    let first = issued
        .into_iter()
        .find_map(|completion| match completion.request {
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
        matches!(
            err.reason,
            LsuRejectReason::QueueFull | LsuRejectReason::Busy
        ),
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
