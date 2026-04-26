use crate::primitives::banked_array::{
    BankedArray, BankedArrayAddressMap, BankedArrayConfig, BankedArrayMemType,
    BankedArrayRejectReason, BankedArrayRequest,
};
use crate::primitives::timeq::ServerConfig;

#[test]
fn banked_array_decode_matches_subbank_then_bank_mapping() {
    let model = BankedArray::<()>::new(BankedArrayConfig {
        num_banks: 4,
        num_subbanks: 16,
        word_bytes: 4,
        address_map: BankedArrayAddressMap::SubbankThenBank,
        ..BankedArrayConfig::default()
    })
    .expect("valid config");

    assert_eq!((0, 0), model.decode_addr(0));
    assert_eq!((0, 1), model.decode_addr(4));
    assert_eq!((1, 0), model.decode_addr(4 * 16));
}

#[test]
fn banked_array_decode_matches_bank_then_subbank_mapping() {
    let model = BankedArray::<()>::new(BankedArrayConfig {
        num_banks: 4,
        num_subbanks: 16,
        word_bytes: 4,
        address_map: BankedArrayAddressMap::BankThenSubbank,
        ..BankedArrayConfig::default()
    })
    .expect("valid config");

    assert_eq!((0, 0), model.decode_addr(0));
    assert_eq!((1, 0), model.decode_addr(4));
    assert_eq!((0, 1), model.decode_addr(4 * 4));
}

#[test]
fn banked_array_rejects_invalid_overrides() {
    let mut model = BankedArray::<u32>::new(BankedArrayConfig::default()).expect("valid config");
    let mut req = BankedArrayRequest::new(0, 4, false, 1);
    req.bank_override = Some(99);
    let reject = model.issue(0, req).expect_err("invalid bank override");
    assert_eq!(BankedArrayRejectReason::InvalidBank, reject.reason);
}

#[test]
fn banked_array_rejects_when_read_queue_is_full() {
    let cfg = BankedArrayConfig {
        num_banks: 1,
        num_subbanks: 1,
        mem_type: BankedArrayMemType::TwoPort,
        read_server: ServerConfig {
            queue_capacity: 1,
            bytes_per_cycle: 1,
            ..ServerConfig::default()
        },
        ..BankedArrayConfig::default()
    };
    let mut model = BankedArray::<u32>::new(cfg).expect("valid config");
    model
        .issue(0, BankedArrayRequest::new(0, 4, false, 1))
        .expect("first issue");
    let reject = model
        .issue(0, BankedArrayRequest::new(0, 4, false, 2))
        .expect_err("queue should be full");
    assert_eq!(BankedArrayRejectReason::QueueFull, reject.reason);
}
