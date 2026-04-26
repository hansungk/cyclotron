use super::*;

fn make_server(config: ServerConfig) -> TimedServer<&'static str> {
    TimedServer::new(config)
}

#[test]
// Make sure the service time calculation combines both the base latency and the bandwidth
fn ticket_ready_time_respects_latency_and_bandwidth() {
    let mut server = make_server(ServerConfig {
        base_latency: 3,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let ticket = server
        .try_enqueue(0, ServiceRequest::new("req0", 8))
        .expect("first request should be accepted");

    assert_eq!(0, ticket.issued_at());
    assert_eq!(8, ticket.size_bytes());
    // service time = base_latency (3) + ceil(8 / 4) = 5 cycles total
    assert_eq!(5, ticket.ready_at());
    // second request pipelines one cycle behind the first
    let ticket1 = server
        .try_enqueue(1, ServiceRequest::new("req1", 4))
        .expect("second request should be accepted");
    assert_eq!(6, ticket1.ready_at());
}

#[test]
// Make sure that the server rejects requests when the queue is full
fn queue_full_is_reported_when_capacity_reached() {
    let mut server = make_server(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 8,
        queue_capacity: 1,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 16))
        .expect("first request should be accepted");

    match server.try_enqueue(0, ServiceRequest::new("req1", 16)) {
        Err(Backpressure::QueueFull { capacity, .. }) => assert_eq!(1, capacity),
        other => panic!("expected queue full backpressure, got {other:?}"),
    }
}

#[test]
// Make sure that completed requests are properly removed from the queue
fn service_ready_drains_completed_requests() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .unwrap();
    server
        .try_enqueue(1, ServiceRequest::new("req1", 4))
        .unwrap();

    let mut collected = Vec::new();
    server.service_ready(1, |res| collected.push(res.payload));
    assert!(collected.is_empty(), "no requests ready yet");

    server.service_ready(2, |res| collected.push(res.payload));
    assert_eq!(vec!["req0"], collected);

    server.service_ready(3, |res| collected.push(res.payload));
    assert_eq!(vec!["req0", "req1"], collected);

    assert_eq!(3, server.available_at());
}

#[test]
// Make sure that servers can model warm up delays
// This will expect the Busy backpressure as the queue is empty but the server is not ready
fn busy_backpressure_allows_warmup_modeling() {
    let mut server = make_server(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    server.set_warmup_until(5);

    match server.try_enqueue(0, ServiceRequest::new("req0", 4)) {
        Err(Backpressure::Busy { available_at, .. }) => assert_eq!(5, available_at),
        other => panic!("expected busy backpressure, got {other:?}"),
    }

    let ticket = server
        .try_enqueue(6, ServiceRequest::new("req0", 4))
        .expect("server should accept once warm-up passes");
    assert_eq!(7, ticket.ready_at());
}

#[test]
// Make sure that zero-byte requests only incur base latency and no throughput delay
fn zero_byte_requests_complete_with_base_latency_only() {
    let mut server = make_server(ServerConfig {
        base_latency: 5,
        bytes_per_cycle: 8,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let ticket = server
        .try_enqueue(10, ServiceRequest::new("zero_bytes", 0))
        .expect("zero byte request should be accepted");

    assert_eq!(10, ticket.issued_at());
    assert_eq!(0, ticket.size_bytes());
    // Service time = base_latency (5) + ceil(0/8) = 5 + 0 = 5 cycles
    assert_eq!(15, ticket.ready_at());

    // Second zero-byte request should pipeline right after the first
    let ticket2 = server
        .try_enqueue(11, ServiceRequest::new("zero_bytes2", 0))
        .expect("second zero byte request should be accepted");
    assert_eq!(16, ticket2.ready_at());
}

#[test]
// If we call service_ready() multiple times at the same cycle, it doesnt double drain the FIFO
fn multiple_service_ready_calls_are_idempotent() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .unwrap();
    server
        .try_enqueue(1, ServiceRequest::new("req1", 4))
        .unwrap();

    let mut collected = Vec::new();

    // First call at cycle 2 should drain req0
    server.service_ready(2, |res| collected.push(res.payload));
    assert_eq!(vec!["req0"], collected);

    // Second call at same cycle should be idempotent
    server.service_ready(2, |res| collected.push(res.payload));
    assert_eq!(vec!["req0"], collected);

    // Call at cycle 3 should drain req1
    server.service_ready(3, |res| collected.push(res.payload));
    assert_eq!(vec!["req0", "req1"], collected);

    // Multiple calls at cycle 5+ should be idempotent
    server.service_ready(5, |res| collected.push(res.payload));
    server.service_ready(6, |res| collected.push(res.payload));
    assert_eq!(vec!["req0", "req1"], collected);
}

#[test]
// Make sure rejected requests can be retried later
fn requests_can_be_retried_after_backpressure() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 1,
        ..ServerConfig::default()
    });

    // Fill the queue with one request
    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .expect("first request should be accepted");

    // Second request should be rejected with QueueFull
    let rejected_request = match server.try_enqueue(0, ServiceRequest::new("req1", 8)) {
        Err(backpressure) => backpressure.into_request(),
        Ok(_) => panic!("expected queue full backpressure"),
    };

    // Verify the rejected request can be recovered
    assert_eq!("req1", rejected_request.payload);
    assert_eq!(8, rejected_request.size_bytes);

    // Drain the completed request to make space
    let mut drained = Vec::new();
    server.service_ready(3, |res| drained.push(res.payload));
    assert_eq!(vec!["req0"], drained);

    // Now retry the previously rejected request
    let ticket = server
        .try_enqueue(3, rejected_request)
        .expect("retried request should be accepted after queue drains");
    assert_eq!(6, ticket.ready_at()); // 3 + 1 + ceil(8/4) = 6
}

#[test]
fn variable_size_requests_compute_correct_service_time() {
    let mut server = make_server(ServerConfig {
        base_latency: 3,
        bytes_per_cycle: 4,
        queue_capacity: 8,
        ..ServerConfig::default()
    });

    let sizes = [1u32, 4, 16, 64, 128];
    for (idx, size) in sizes.iter().enumerate() {
        let now = (idx as u64) * 100;
        let ticket = server
            .try_enqueue(now, ServiceRequest::new("req", *size))
            .expect("request should be accepted");
        let service = ((*size as u64) + 3) / 4;
        let expected = now + 3 + service;
        assert_eq!(expected, ticket.ready_at());
    }
}

#[test]
fn enqueue_at_exact_warmup_boundary() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });
    server.set_warmup_until(100);

    let ticket = server
        .try_enqueue(100, ServiceRequest::new("req", 4))
        .expect("enqueue at warmup boundary should succeed");
    assert_eq!(102, ticket.ready_at());
}

#[test]
fn completions_per_cycle_equals_one_with_many_ready() {
    let mut server = make_server(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 8,
        queue_capacity: 16,
        completions_per_cycle: 1,
        ..ServerConfig::default()
    });

    for _ in 0..5 {
        server
            .try_enqueue(0, ServiceRequest::new("req", 0))
            .expect("enqueue should succeed");
    }

    server.advance_ready(0);
    let mut ready = Vec::new();
    server.service_ready(0, |res| ready.push(res.payload));
    assert_eq!(1, ready.len(), "only one completion per cycle");
    server.service_ready(0, |res| ready.push(res.payload));
    assert_eq!(1, ready.len(), "same cycle should not drain more");
}

#[test]
fn advance_ready_moves_to_ready_queue() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let ticket = server
        .try_enqueue(0, ServiceRequest::new("req", 4))
        .expect("enqueue should succeed");
    assert!(server.peek_ready(0).is_none());
    server.advance_ready(ticket.ready_at());
    assert!(server.peek_ready(ticket.ready_at()).is_some());
}

#[test]
fn back_to_back_requests_pipeline_correctly() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 8,
        ..ServerConfig::default()
    });

    let mut ready = Vec::new();
    for cycle in 0..5 {
        let ticket = server
            .try_enqueue(cycle, ServiceRequest::new("req", 4))
            .expect("enqueue should succeed");
        ready.push(ticket.ready_at());
    }

    assert_eq!(ready, vec![2, 3, 4, 5, 6]);
}

#[test]
fn single_byte_request_at_cycle_zero() {
    let mut server = make_server(ServerConfig {
        base_latency: 5,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let ticket = server
        .try_enqueue(0, ServiceRequest::new("req", 1))
        .expect("enqueue should succeed");
    assert_eq!(6, ticket.ready_at());
}

#[test]
fn max_capacity_then_drain_one_then_enqueue() {
    let mut server = make_server(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    server.try_enqueue(0, ServiceRequest::new("a", 4)).unwrap();
    server.try_enqueue(0, ServiceRequest::new("b", 4)).unwrap();
    assert!(server.try_enqueue(0, ServiceRequest::new("c", 4)).is_err());

    server.advance_ready(1);
    assert!(server.pop_ready(1).is_some());
    assert!(server.try_enqueue(1, ServiceRequest::new("c", 4)).is_ok());
}

#[test]
fn high_throughput_saturated_queue() {
    let mut server = TimedServer::<u32>::new(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 1024,
        queue_capacity: 1024,
        ..ServerConfig::default()
    });

    for id in 0..1024u32 {
        server
            .try_enqueue(0, ServiceRequest::new(id, 1))
            .expect("enqueue should succeed");
    }

    let mut seen = Vec::new();
    for cycle in 1..=1024u64 {
        while let Some(result) = server.pop_ready(cycle) {
            seen.push(result.payload);
        }
    }
    assert_eq!(seen.len(), 1024);
    for (idx, val) in seen.into_iter().enumerate() {
        assert_eq!(val as usize, idx);
    }
}

#[test]
// Rejected requests due to Busy signal can be retried later
fn busy_backpressure_can_also_be_retried() {
    let mut server = make_server(ServerConfig {
        base_latency: 2,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    server.set_warmup_until(10);

    // Request at cycle 5 should be rejected with Busy
    let rejected_request = match server.try_enqueue(5, ServiceRequest::new("busy_req", 4)) {
        Err(Backpressure::Busy {
            request,
            available_at,
        }) => {
            assert_eq!(10, available_at);
            request
        }
        other => panic!("expected busy backpressure, got {other:?}"),
    };

    // Retry the request after the busy period
    let ticket = server
        .try_enqueue(10, rejected_request)
        .expect("retried request should be accepted after busy period");
    assert_eq!(13, ticket.ready_at()); // 10 + 2 + ceil(4/4) = 13
}

#[test]
// If we have huge cycle numbers, make sure we don't overflow when computing ready_at
fn large_cycles_dont_overflow() {
    let mut server = make_server(ServerConfig {
        base_latency: 1000,
        bytes_per_cycle: 1,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    // Use a very large cycle number close to u64::MAX
    let large_cycle = u64::MAX - 2000;

    let ticket = server
        .try_enqueue(large_cycle, ServiceRequest::new("large_cycle", 100))
        .expect("large cycle request should be accepted");

    assert_eq!(large_cycle, ticket.issued_at());
    // Service time = 1000 + ceil(100/1) = 1100
    // Should not overflow: large_cycle + 1100 should be valid
    assert!(ticket.ready_at() > large_cycle);
    assert!(ticket.ready_at() <= u64::MAX);
}

#[test]
// Make sure arithmetic operations saturate rather than overflows
fn saturating_arithmetic_prevents_overflow() {
    let mut server = make_server(ServerConfig {
        base_latency: 1000,
        bytes_per_cycle: 1,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let ticket = server
        .try_enqueue(u64::MAX - 400, ServiceRequest::new("near_max", 1000))
        .expect("near max cycle request should be accepted when server not busy");

    assert_eq!(u64::MAX, ticket.ready_at());
}

#[test]
// Some edge cases for remaining_cycles (exactly ready, past ready)
fn ticket_remaining_cycles_handles_edge_cases() {
    let ticket = Ticket::new(100, 150, 32);

    // Normal case: ticket not ready yet
    assert_eq!(20, ticket.remaining_cycles(130));

    // Edge case: exactly ready
    assert_eq!(0, ticket.remaining_cycles(150));

    // Edge case: past ready time (should saturate to 0)
    assert_eq!(0, ticket.remaining_cycles(200));

    // Edge case: way past ready time
    assert_eq!(0, ticket.remaining_cycles(u64::MAX));
}

#[test]
// Verify that peek_ready allows observing completions without consuming them,
// and pop_ready actually removes them from the queue
fn peek_and_pop_ready_allow_backpressure_control() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .unwrap();

    // Nothing ready yet.
    assert!(server.peek_ready(1).is_none());

    // Once the request matures, peek exposes it without consuming.
    let peeked = server.peek_ready(5).expect("result should be ready");
    assert_eq!("req0", peeked.payload);

    // The item stays pending until pop_ready is invoked.
    assert!(server.peek_ready(5).is_some());

    let popped = server
        .pop_ready(5)
        .expect("result should be popped successfully");
    assert_eq!("req0", popped.payload);

    // After popping, there is no outstanding work.
    assert!(server.pop_ready(5).is_none());
    assert_eq!(server.outstanding(), 0);
}

#[test]
// Ensure that multiple requests can be in-flight simultaneously and pipeline correctly,
// with each request starting service as soon as throughput allows
fn pipelined_requests_overlap() {
    let mut server = make_server(ServerConfig {
        base_latency: 2,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        ..ServerConfig::default()
    });

    let t0 = server
        .try_enqueue(0, ServiceRequest::new("req0", 8))
        .unwrap();
    let t1 = server
        .try_enqueue(1, ServiceRequest::new("req1", 8))
        .unwrap();
    let t2 = server
        .try_enqueue(2, ServiceRequest::new("req2", 8))
        .unwrap();

    assert_eq!(0, t0.issued_at());
    assert_eq!(4, t0.ready_at()); // 2 + ceil(8/4)=4
    assert_eq!(6, t1.ready_at()); // launches two cycles later while req0 still pending
    assert_eq!(8, t2.ready_at());
}

#[test]
// Verify that completions_per_cycle limits how many results can be surfaced in a single cycle,
// even if multiple requests have already finished processing
fn completion_bandwidth_limits_visible_results() {
    let mut server = make_server(ServerConfig {
        base_latency: 0,
        bytes_per_cycle: 4,
        queue_capacity: 4,
        completions_per_cycle: 1,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .unwrap();
    server
        .try_enqueue(0, ServiceRequest::new("req1", 4))
        .unwrap();

    // Both requests mature by cycle 1, but only one completion can be surfaced per cycle
    assert!(server.pop_ready(1).is_some());
    // Second completion becomes visible on the next cycle
    assert!(server.pop_ready(1).is_none());
    assert!(server.pop_ready(2).is_some());
}

#[test]
// Ensure that ServerStats correctly tracks request issuance, completion,
// byte counts, and maximum outstanding queue depth
fn server_stats_track_activity() {
    let mut server = make_server(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 2,
        ..ServerConfig::default()
    });

    server
        .try_enqueue(0, ServiceRequest::new("req0", 4))
        .unwrap();
    server
        .try_enqueue(1, ServiceRequest::new("req1", 8))
        .unwrap();

    server.pop_ready(2);
    server.pop_ready(4);

    let stats = server.stats();
    assert_eq!(2, stats.issued);
    assert_eq!(2, stats.completed);
    assert_eq!(12, stats.bytes_issued);
    assert_eq!(12, stats.bytes_completed);
    assert_eq!(2, stats.max_outstanding);
}
