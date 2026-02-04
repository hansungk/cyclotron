use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct GmemPolicyConfig {
    pub l0_enabled: bool,
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
    pub flush_bytes: u32,
    pub seed: u64,
}

impl Default for GmemPolicyConfig {
    fn default() -> Self {
        let s = Self {
            l0_enabled: true,
            l1_writeback_rate: 0.1,
            l2_writeback_rate: 0.1,
            l0_line_bytes: 64,
            l1_line_bytes: 32,
            l2_line_bytes: 32,
            l0_sets: 512,
            l0_ways: 1,
            l1_sets: 512,
            l1_ways: 4,
            l2_sets: 512,
            l2_ways: 8,
            // See radiance/src/main/scala/radiance/muon/MuonTile.scala:
            // L0i flush is at peripheralAddr; L0d flush is at peripheralAddr + 0x100.
            // Radiance memory map allocates 0x200 bytes per core for these registers,
            // so L0d flush MMIO is base=0x0008_0300 stride=0x200.
            l0_flush_mmio_base: 0x0008_0300,
            l0_flush_mmio_stride: 0x200,
            l0_flush_mmio_size: 0x100,
            flush_bytes: 4096,
            seed: 0,
        };
        s.ensure_valid();
        s
    }
}

impl GmemPolicyConfig {
    /// Ensure the config has sensible, non-zero values for fields
    /// that must be positive. This centralizes validation so callers
    /// can choose to fail fast on bad configs instead of scattering
    /// `.max(1)` defensive clamping across the codebase.
    pub fn ensure_valid(&self) {
        assert!(self.l0_line_bytes > 0, "l0_line_bytes must be > 0");
        assert!(self.l1_line_bytes > 0, "l1_line_bytes must be > 0");
        assert!(self.l2_line_bytes > 0, "l2_line_bytes must be > 0");
        assert!(self.l0_sets > 0, "l0_sets must be > 0");
        assert!(self.l1_sets > 0, "l1_sets must be > 0");
        assert!(self.l2_sets > 0, "l2_sets must be > 0");
        assert!(self.l0_ways > 0, "l0_ways must be > 0");
        assert!(self.l1_ways > 0, "l1_ways must be > 0");
        assert!(self.l2_ways > 0, "l2_ways must be > 0");
        assert!(self.flush_bytes > 0, "flush_bytes must be > 0");
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

