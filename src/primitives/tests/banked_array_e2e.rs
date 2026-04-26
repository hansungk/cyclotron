use crate::primitives::banked_array::{BankedArray, BankedArrayConfig, BankedArrayRequest};
use crate::primitives::timeq::ServerConfig;

#[test]
fn banked_array_end_to_end_serializes_same_direction_same_target() {
    let cfg = BankedArrayConfig {
        num_banks: 1,
        num_subbanks: 1,
        read_server: ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        },
        write_server: ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        },
        ..BankedArrayConfig::default()
    };
    let mut model = BankedArray::<&'static str>::new(cfg).expect("valid config");

    let read0 = model
        .issue(0, BankedArrayRequest::new(0, 4, false, "read0"))
        .expect("issue read0");
    let read1 = model
        .issue(0, BankedArrayRequest::new(0, 4, false, "read1"))
        .expect("issue read1");
    let write0 = model
        .issue(0, BankedArrayRequest::new(0, 4, true, "write0"))
        .expect("issue write0");

    assert_eq!(2, read0.ticket.ready_at());
    assert_eq!(3, read1.ticket.ready_at()); // same read port, one cycle later
    assert_eq!(2, write0.ticket.ready_at()); // write pipeline is independent

    let mut done = Vec::new();
    for now in 0..=6 {
        model.collect_completions(now);
        while let Some(c) = model.pop_completion() {
            done.push((c.request.payload, c.completed_at));
        }
    }

    assert_eq!(3, done.len());
    assert_eq!(("read0", 2), done[0]);
    assert_eq!(("write0", 2), done[1]);
    assert_eq!(("read1", 3), done[2]);
}

#[test]
fn banked_array_end_to_end_supports_bank_parallelism() {
    let cfg = BankedArrayConfig {
        num_banks: 2,
        num_subbanks: 1,
        read_server: ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        },
        ..BankedArrayConfig::default()
    };
    let mut model = BankedArray::<u32>::new(cfg).expect("valid config");

    // address 0 -> bank 0, address 4 -> bank 1 for SubbankThenBank with num_subbanks=1
    let a = model
        .issue(0, BankedArrayRequest::new(0, 4, false, 10))
        .expect("issue bank0");
    let b = model
        .issue(0, BankedArrayRequest::new(4, 4, false, 11))
        .expect("issue bank1");
    assert_eq!(1, a.ticket.ready_at());
    assert_eq!(1, b.ticket.ready_at());

    model.collect_completions(1);
    let mut payloads = Vec::new();
    while let Some(c) = model.pop_completion() {
        payloads.push(c.request.payload);
    }
    payloads.sort_unstable();
    assert_eq!(vec![10, 11], payloads);
}
