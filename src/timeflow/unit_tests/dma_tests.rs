use crate::timeflow::dma::{DmaConfig, DmaQueue};

#[test]
fn dma_queue_completes() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 1;
    let mut dma = DmaQueue::new(cfg);

    let ticket = dma.try_issue(0, 128).expect("issue");
    dma.tick(ticket.ready_at().saturating_sub(1));
    assert_eq!(dma.completed(), 0);
    dma.tick(ticket.ready_at());
    assert_eq!(dma.completed(), 1);
}

#[test]
fn disabled_dma_immediate() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = false;
    let mut dma = DmaQueue::new(cfg);
    let ticket = dma.try_issue(5, 64).expect("issue");
    assert_eq!(ticket.ready_at(), 5);
}

#[test]
fn queue_full_rejects() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = true;
    cfg.queue.queue_capacity = 1;
    let mut dma = DmaQueue::new(cfg);
    assert!(dma.try_issue(0, 64).is_ok());
    let err = dma.try_issue(0, 64).expect_err("queue full");
    assert_eq!(crate::timeflow::dma::DmaRejectReason::QueueFull, err.reason);
}

#[test]
fn zero_byte_transfer_uses_base_latency() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 3;
    cfg.queue.bytes_per_cycle = 4;
    let mut dma = DmaQueue::new(cfg);
    let ticket = dma.try_issue(10, 0).unwrap();
    assert_eq!(13, ticket.ready_at());
}

#[test]
fn dma_large_transfer_bandwidth_limited() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 2;
    cfg.queue.bytes_per_cycle = 8;
    let mut dma = DmaQueue::new(cfg);

    let ticket = dma.try_issue(0, 64).unwrap();
    assert_eq!(10, ticket.ready_at());
}

#[test]
fn dma_very_large_transfer_no_overflow() {
    let mut cfg = DmaConfig::default();
    cfg.enabled = true;
    cfg.queue.base_latency = 1;
    cfg.queue.bytes_per_cycle = 1;
    let mut dma = DmaQueue::new(cfg);

    let ticket = dma.try_issue(0, u32::MAX).unwrap();
    assert_eq!(u32::MAX as u64 + 1, ticket.ready_at());
}
