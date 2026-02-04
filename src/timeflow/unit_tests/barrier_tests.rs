use crate::timeflow::barrier::{BarrierConfig, BarrierManager};

#[test]
fn barrier_releases_after_all_warps_arrive() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(2);
    cfg.queue.base_latency = 2;

    let mut barrier = BarrierManager::new(cfg, 2);
    assert!(barrier.arrive(0, 0, 0).is_none());
    let release_at = barrier.arrive(0, 1, 0).expect("barrier should schedule");
    assert!(release_at >= 2);

    assert!(barrier.tick(1).is_none());
    let released = barrier.tick(release_at).expect("barrier should release");
    assert_eq!(released.len(), 2);
}

#[test]
fn barrier_tracks_multiple_ids() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(2);
    cfg.queue.base_latency = 1;

    let mut barrier = BarrierManager::new(cfg, 2);
    assert!(barrier.arrive(0, 0, 1).is_none());
    assert!(barrier.arrive(0, 1, 2).is_none());

    let rel0 = barrier.arrive(0, 1, 1).expect("barrier 1 should schedule");
    let rel1 = barrier.arrive(0, 0, 2).expect("barrier 2 should schedule");
    let cycle = rel0.min(rel1);
    let released = barrier.tick(cycle).unwrap_or_default();
    assert!(!released.is_empty(), "expected some warps released");
}

#[test]
fn single_warp_barrier_immediate() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(1);
    cfg.queue.base_latency = 0;

    let mut barrier = BarrierManager::new(cfg, 1);
    let release_at = barrier.arrive(0, 0, 0).expect("should schedule release");
    let released = barrier.tick(release_at).expect("should release");
    assert_eq!(released, vec![0]);
}

#[test]
fn partial_arrival_waits() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(4);
    cfg.queue.base_latency = 1;

    let mut barrier = BarrierManager::new(cfg, 4);
    assert!(barrier.arrive(0, 0, 0).is_none());
    assert!(barrier.arrive(0, 1, 0).is_none());
    assert!(barrier.tick(2).is_none());
}

#[test]
fn disabled_barrier_passthrough() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = false;
    cfg.expected_warps = Some(4);

    let mut barrier = BarrierManager::new(cfg, 4);
    let release = barrier.arrive(0, 0, 0);
    assert_eq!(release, Some(0));
}

#[test]
fn warp_arrives_twice_same_barrier() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(2);
    cfg.queue.base_latency = 1;

    let mut barrier = BarrierManager::new(cfg, 2);
    assert!(barrier.arrive(0, 0, 0).is_none());
    assert!(barrier.arrive(0, 0, 0).is_none());
    let release_at = barrier.arrive(0, 1, 0).expect("should schedule");
    let released = barrier.tick(release_at).expect("release expected");
    assert_eq!(released.len(), 2);
}

#[test]
fn expected_warps_exceeds_num_warps() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(8);
    cfg.queue.base_latency = 0;

    let mut barrier = BarrierManager::new(cfg, 4);
    for warp in 0..4 {
        let maybe = barrier.arrive(0, warp, 0);
        if warp < 3 {
            assert!(maybe.is_none());
        } else {
            assert!(maybe.is_some());
        }
    }
}

#[test]
fn rapid_barrier_cycles() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(2);
    cfg.queue.base_latency = 0;

    let mut barrier = BarrierManager::new(cfg, 2);
    for cycle in 0..100 {
        assert!(barrier.arrive(cycle, 0, 0).is_none());
        let release_at = barrier.arrive(cycle, 1, 0).expect("schedule");
        let released = barrier.tick(release_at).expect("release");
        assert_eq!(released.len(), 2);
    }
}

#[test]
fn barrier_with_different_ids_interleaved() {
    let mut cfg = BarrierConfig::default();
    cfg.enabled = true;
    cfg.expected_warps = Some(2);
    cfg.queue.base_latency = 0;

    let mut barrier = BarrierManager::new(cfg, 2);
    assert!(barrier.arrive(0, 0, 1).is_none());
    assert!(barrier.arrive(0, 0, 2).is_none());

    let rel1 = barrier.arrive(0, 1, 1).expect("barrier 1 schedule");
    let rel2 = barrier.arrive(0, 1, 2).expect("barrier 2 schedule");
    let mut released = Vec::new();
    if let Some(mut warps) = barrier.tick(rel1) {
        released.append(&mut warps);
    }
    if let Some(mut warps) = barrier.tick(rel2.max(rel1.saturating_add(1))) {
        released.append(&mut warps);
    }
    assert_eq!(released.len(), 4);
}
