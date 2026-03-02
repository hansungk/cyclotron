use crate::timeflow::gmem::policy::bank_for;
use std::collections::HashSet;

#[test]
fn bank_mapping_is_stable_for_same_line() {
    let bank0 = bank_for(42, 4, 0x1111_2222_3333_4444);
    let bank1 = bank_for(42, 4, 0x1111_2222_3333_4444);
    assert_eq!(bank0, bank1);
}

#[test]
fn bank_mapping_varies_across_lines() {
    let mut seen = HashSet::new();
    for line in 0..16 {
        seen.insert(bank_for(line, 4, 0x5555_6666_7777_8888));
    }
    assert!(seen.len() > 1, "expected multiple banks to be used");
}

#[test]
fn hash_is_deterministic() {
    use crate::timeflow::gmem::policy::hash_u64;
    assert_eq!(hash_u64(1234), hash_u64(1234));
}

#[test]
fn bank_for_handles_zero_line() {
    let bank = bank_for(0, 4, 0x1111_2222_3333_4444);
    assert!(bank < 4);
}

#[test]
fn bank_for_handles_max_line() {
    let bank = bank_for(u64::MAX, 8, 0x5555_6666_7777_8888);
    assert!(bank < 8);
}

#[test]
fn bank_distribution_is_uniform() {
    let banks = 4u64;
    let mut counts = [0usize; 4];
    for line in 0..4000u64 {
        let bank = bank_for(line, banks, 0xdead_beef_cafe_babe);
        counts[bank] += 1;
    }
    for &count in &counts {
        assert!(count > 800, "bank too sparse: {count}");
        assert!(count < 1200, "bank too dense: {count}");
    }
}
