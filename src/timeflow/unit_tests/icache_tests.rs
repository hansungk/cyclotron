use crate::timeflow::icache::{IcacheFlowConfig, IcacheRequest, IcacheSubgraph};

#[test]
fn icache_hit_ready_at_now_with_zero_latency() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 1.0;
    cfg.hit.base_latency = 0;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x1000, 8);
    let issue = icache.issue(10, req).expect("hit should accept");
    assert_eq!(issue.ticket.ready_at(), 10);
}

#[test]
fn icache_miss_latency_applies() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 0.0;
    cfg.miss.base_latency = 7;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x1000, 8);
    let issue = icache.issue(5, req).expect("miss should accept");
    assert_eq!(issue.ticket.ready_at(), 12);
}

#[test]
fn icache_queue_full_rejects() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 0.0;
    cfg.miss.queue_capacity = 1;
    let mut icache = IcacheSubgraph::new(cfg);
    let req0 = IcacheRequest::new(0, 0x1000, 8);
    icache.issue(0, req0).expect("first miss should accept");
    let req1 = IcacheRequest::new(1, 0x2000, 8);
    let err = icache.issue(0, req1).expect_err("second miss should reject");
    assert!(err.retry_at > 0);
}

#[test]
fn icache_hit_rate_one_always_hits() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 1.0;
    cfg.hit.base_latency = 1;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x2000, 8);
    let issue = icache.issue(10, req).expect("hit should accept");
    assert_eq!(11, issue.ticket.ready_at());
}

#[test]
fn icache_hit_rate_zero_always_misses() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 0.0;
    cfg.miss.base_latency = 5;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x2000, 8);
    let issue = icache.issue(3, req).expect("miss should accept");
    assert_eq!(8, issue.ticket.ready_at());
}

#[test]
fn icache_line_address_computed_correctly() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 1.0;
    cfg.policy.line_bytes = 32;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x1040, 8);
    let issue = icache.issue(0, req).expect("hit should accept");
    icache.tick(issue.ticket.ready_at());
    let stats = icache.stats();
    assert_eq!(1, stats.issued);
    assert_eq!(1, stats.hits);
}

#[test]
fn icache_stats_track_hits_and_misses() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 0.0;
    let mut icache = IcacheSubgraph::new(cfg);
    let req = IcacheRequest::new(0, 0x1000, 8);
    let issue = icache.issue(0, req).expect("miss should accept");
    icache.tick(issue.ticket.ready_at());
    let stats = icache.stats();
    assert_eq!(1, stats.issued);
    assert_eq!(1, stats.misses);
    assert_eq!(1, stats.completed);
}

#[test]
fn icache_many_parallel_fetches() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 1.0;
    cfg.hit.queue_capacity = 8;
    cfg.hit.base_latency = 0;
    let mut icache = IcacheSubgraph::new(cfg);

    for warp in 0..8 {
        let req = IcacheRequest::new(warp, 0x1000 + (warp as u32) * 4, 8);
        icache.issue(0, req).expect("issue should succeed");
    }
    icache.tick(0);
    let stats = icache.stats();
    assert_eq!(8, stats.issued);
    assert_eq!(8, stats.completed);
}

#[test]
fn icache_pc_zero_handling() {
    let mut cfg = IcacheFlowConfig::default();
    cfg.policy.hit_rate = 1.0;
    cfg.hit.base_latency = 0;
    let mut icache = IcacheSubgraph::new(cfg);

    let req = IcacheRequest::new(0, 0, 8);
    let issue = icache.issue(0, req).expect("issue should succeed");
    icache.tick(issue.ticket.ready_at());
    let stats = icache.stats();
    assert_eq!(1, stats.completed);
}
