use crate::primitives::interconnect_arbiter::{
    ArbitrationPolicy, Connectivity, InterconnectArbiter, InterconnectConfig,
    InterconnectRejectReason, InterconnectRequest,
};

#[test]
fn interconnect_rejects_invalid_connectivity_matrix_shape() {
    let cfg = InterconnectConfig::simple(2, 2).with_connectivity_matrix(vec![vec![true, true]]);
    assert!(InterconnectArbiter::<u32>::new(cfg).is_err());
}

#[test]
fn interconnect_rejects_disconnected_route() {
    let cfg = InterconnectConfig {
        num_inputs: 2,
        num_outputs: 2,
        connectivity: Connectivity::Matrix(vec![vec![true, false], vec![false, true]]),
        arbitration: ArbitrationPolicy::LowestIndexFirst,
        ..InterconnectConfig::default()
    };
    let mut xbar = InterconnectArbiter::<u32>::new(cfg).expect("valid config");

    let reject = xbar
        .enqueue(0, InterconnectRequest::new(0, 1, 42, 4))
        .expect_err("route should be blocked");
    assert_eq!(InterconnectRejectReason::Disconnected, reject.reason);
}

#[test]
fn interconnect_rejects_input_queue_overflow() {
    let cfg = InterconnectConfig {
        input_queue_capacity: 1,
        ..InterconnectConfig::simple(1, 1)
    };
    let mut xbar = InterconnectArbiter::<u32>::new(cfg).expect("valid config");

    xbar.enqueue(0, InterconnectRequest::new(0, 0, 1, 4))
        .expect("first request");
    let reject = xbar
        .enqueue(0, InterconnectRequest::new(0, 0, 2, 4))
        .expect_err("second request should overflow");
    assert_eq!(InterconnectRejectReason::InputQueueFull, reject.reason);
}
