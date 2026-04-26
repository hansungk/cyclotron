use crate::primitives::timeq::{ServerConfig, ServiceRequest, TimedServer};

#[test]
fn timed_server_end_to_end_pipeline_completes_in_order() {
    let mut server = TimedServer::new(ServerConfig {
        base_latency: 1,
        bytes_per_cycle: 4,
        queue_capacity: 8,
        ..ServerConfig::default()
    });

    let t0 = server
        .try_enqueue(0, ServiceRequest::new("r0", 8))
        .expect("enqueue r0");
    let t1 = server
        .try_enqueue(1, ServiceRequest::new("r1", 4))
        .expect("enqueue r1");
    let t2 = server
        .try_enqueue(2, ServiceRequest::new("r2", 4))
        .expect("enqueue r2");

    assert_eq!(3, t0.ready_at()); // start 0 + base 1 + ceil(8/4)=2
    assert_eq!(4, t1.ready_at()); // start 2 + base 1 + 1
    assert_eq!(5, t2.ready_at()); // start 3 + base 1 + 1

    let mut completions = Vec::new();
    for now in 0..=6 {
        server.service_ready(now, |result| {
            completions.push((result.payload, now, result.ticket.ready_at()));
        });
    }

    assert_eq!(3, completions.len());
    assert_eq!("r0", completions[0].0);
    assert_eq!("r1", completions[1].0);
    assert_eq!("r2", completions[2].0);
    assert_eq!(3, completions[0].1);
    assert_eq!(4, completions[1].1);
    assert_eq!(5, completions[2].1);
}
