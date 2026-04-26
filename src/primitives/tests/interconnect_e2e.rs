use crate::primitives::interconnect_arbiter::{
    ArbitrationPolicy, InterconnectArbiter, InterconnectConfig, InterconnectRequest,
};
use crate::primitives::timeq::ServerConfig;

#[test]
fn interconnect_round_robin_end_to_end_alternates_winners() {
    let cfg = InterconnectConfig {
        num_inputs: 2,
        num_outputs: 1,
        input_queue_capacity: 16,
        grants_per_output_per_cycle: 1,
        arbitration: ArbitrationPolicy::RoundRobin,
        output_server: ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 1,
            queue_capacity: 16,
            ..ServerConfig::default()
        },
        ..InterconnectConfig::default()
    };

    let mut xbar = InterconnectArbiter::new(cfg).expect("valid config");
    for seq in 0..4 {
        xbar.enqueue(0, InterconnectRequest::new(0, 0, (0usize, seq), 1))
            .expect("enqueue input0");
        xbar.enqueue(0, InterconnectRequest::new(1, 0, (1usize, seq), 1))
            .expect("enqueue input1");
    }

    let mut winners = Vec::new();
    for now in 0..20 {
        xbar.step(now);
        while let Some(done) = xbar.pop_completion() {
            winners.push(done.request.payload.0);
        }
        if winners.len() == 8 {
            break;
        }
    }

    assert_eq!(8, winners.len());
    assert_eq!(vec![0, 1, 0, 1, 0, 1, 0, 1], winners);
}

#[test]
fn interconnect_end_to_end_supports_multiple_outputs() {
    let cfg = InterconnectConfig {
        num_inputs: 2,
        num_outputs: 2,
        input_queue_capacity: 8,
        grants_per_output_per_cycle: 1,
        output_server: ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        },
        ..InterconnectConfig::default()
    };
    let mut xbar = InterconnectArbiter::new(cfg).expect("valid config");

    xbar.enqueue(0, InterconnectRequest::new(0, 0, "a0", 4))
        .expect("enqueue a0");
    xbar.enqueue(0, InterconnectRequest::new(1, 1, "b0", 4))
        .expect("enqueue b0");

    let mut completions = Vec::new();
    for now in 0..8 {
        xbar.step(now);
        while let Some(done) = xbar.pop_completion() {
            completions.push((done.request.payload, done.completed_at));
        }
        if completions.len() == 2 {
            break;
        }
    }

    assert_eq!(2, completions.len());
    // both outputs can grant in the same cycle and complete together here
    assert_eq!(2, completions[0].1);
    assert_eq!(2, completions[1].1);
}
