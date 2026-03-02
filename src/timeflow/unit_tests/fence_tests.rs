use crate::timeflow::fence::{FenceConfig, FenceQueue, FenceRequest};

#[test]
fn fence_queue_delays_release() {
    let mut cfg = FenceConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 2;
    cfg.queue.queue_capacity = 1;
    let mut fence = FenceQueue::new(cfg);

    assert!(fence
        .try_issue(
            0,
            FenceRequest {
                warp: 0,
                request_id: 1
            }
        )
        .is_ok());
    fence.tick(1);
    assert!(fence.pop_ready().is_none());
    fence.tick(2);
    assert!(fence.pop_ready().is_some());
}

#[test]
fn disabled_fence_immediate() {
    let mut cfg = FenceConfig::default();
    cfg.enabled = false;
    let mut fence = FenceQueue::new(cfg);
    fence
        .try_issue(
            5,
            FenceRequest {
                warp: 0,
                request_id: 1,
            },
        )
        .unwrap();
    assert!(fence.pop_ready().is_some());
}

#[test]
fn queue_full_rejects() {
    let mut cfg = FenceConfig::default();
    cfg.enabled = true;
    cfg.queue.queue_capacity = 1;
    let mut fence = FenceQueue::new(cfg);

    fence
        .try_issue(
            0,
            FenceRequest {
                warp: 0,
                request_id: 1,
            },
        )
        .unwrap();
    let err = fence
        .try_issue(
            0,
            FenceRequest {
                warp: 0,
                request_id: 2,
            },
        )
        .expect_err("queue should be full");
    assert_eq!(
        crate::timeflow::fence::FenceRejectReason::QueueFull,
        err.reason
    );
}

#[test]
fn multiple_fences_queue_fifo() {
    let mut cfg = FenceConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 0;
    cfg.queue.queue_capacity = 4;
    let mut fence = FenceQueue::new(cfg);

    fence
        .try_issue(
            0,
            FenceRequest {
                warp: 0,
                request_id: 1,
            },
        )
        .unwrap();
    fence
        .try_issue(
            0,
            FenceRequest {
                warp: 1,
                request_id: 2,
            },
        )
        .unwrap();

    fence.tick(0);
    let first = fence.pop_ready().expect("first ready");
    fence.tick(1);
    let second = fence.pop_ready().expect("second ready");
    assert_eq!(first.request_id, 1);
    assert_eq!(second.request_id, 2);
}

#[test]
fn fence_at_cycle_zero() {
    let mut cfg = FenceConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 0;
    let mut fence = FenceQueue::new(cfg);

    fence
        .try_issue(
            0,
            FenceRequest {
                warp: 0,
                request_id: 1,
            },
        )
        .unwrap();
    fence.tick(0);
    assert!(fence.pop_ready().is_some());
}
