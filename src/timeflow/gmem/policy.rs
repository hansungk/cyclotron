use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct GmemPolicyConfig {
    pub l0_hit_rate: f64,
    pub l1_hit_rate: f64,
    pub l2_hit_rate: f64,
    pub l1_writeback_rate: f64,
    pub l2_writeback_rate: f64,
    pub l0_line_bytes: u32,
    pub l1_line_bytes: u32,
    pub l2_line_bytes: u32,
    pub l0_sets: usize,
    pub l0_ways: usize,
    pub l1_sets: usize,
    pub l1_ways: usize,
    pub l2_sets: usize,
    pub l2_ways: usize,
    pub l0_flush_mmio_base: u64,
    pub l0_flush_mmio_stride: u64,
    pub l0_flush_mmio_size: u64,
    pub l1_banks: usize,
    pub l2_banks: usize,
    pub flush_bytes: u32,
    pub seed: u64,
}

impl Default for GmemPolicyConfig {
    fn default() -> Self {
        Self {
            l0_hit_rate: 0.4,
            l1_hit_rate: 0.7,
            l2_hit_rate: 0.9,
            l1_writeback_rate: 0.1,
            l2_writeback_rate: 0.1,
            l0_line_bytes: 64,
            l1_line_bytes: 32,
            l2_line_bytes: 128,
            l0_sets: 512,
            l0_ways: 1,
            l1_sets: 512,
            l1_ways: 4,
            l2_sets: 512,
            l2_ways: 8,
            l0_flush_mmio_base: 0x0008_0200,
            l0_flush_mmio_stride: 0x100,
            l0_flush_mmio_size: 0x100,
            l1_banks: 2,
            l2_banks: 1,
            flush_bytes: 4096,
            seed: 0,
        }
    }
}

pub(crate) fn line_addr(addr: u64, line_bytes: u32) -> u64 {
    let bytes = line_bytes.max(1) as u64;
    addr / bytes
}

pub(crate) fn bank_for(line_addr: u64, banks: u64, salt: u64) -> usize {
    if banks == 0 {
        return 0;
    }
    (hash_u64(line_addr ^ salt) % banks) as usize
}

pub(crate) fn decide(rate: f64, key: u64) -> bool {
    let clamped = if rate < 0.0 {
        0.0
    } else if rate > 1.0 {
        1.0
    } else {
        rate
    };
    if clamped <= 0.0 {
        return false;
    }
    if clamped >= 1.0 {
        return true;
    }
    let threshold = (clamped * (u64::MAX as f64)) as u64;
    hash_u64(key) <= threshold
}

pub(crate) fn hash_u64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x
}

#[cfg(test)]
mod tests {
    use super::{bank_for, hash_u64};
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
}
