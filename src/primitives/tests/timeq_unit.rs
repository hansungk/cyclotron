use crate::primitives::timeq::{normalize_retry, ServerConfig, ServiceRequest, TimedServer};

#[test]
fn normalize_retry_never_returns_current_cycle() {
    assert_eq!(11, normalize_retry(10, 10));
    assert_eq!(11, normalize_retry(10, 0));
    assert_eq!(17, normalize_retry(10, 17));
}

#[test]
fn timed_server_reports_queue_full_when_capacity_is_reached() {
    let mut server = TimedServer::new(ServerConfig {
        queue_capacity: 1,
        bytes_per_cycle: 4,
        ..ServerConfig::default()
    });

    assert!(server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .is_ok());
    assert!(server
        .try_enqueue(0, ServiceRequest::new("req1", 4))
        .is_err());
}

#[test]
fn timed_server_honors_warmup_busy_window() {
    let mut server = TimedServer::new(ServerConfig {
        warmup_latency: 5,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    assert!(server
        .try_enqueue(0, ServiceRequest::new("too_early", 1))
        .is_err());
    assert!(server
        .try_enqueue(5, ServiceRequest::new("on_time", 1))
        .is_ok());
}
