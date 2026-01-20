use serde::Deserialize;
use crate::sim::config::Config;

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(default)]
pub struct MuonConfig {
    pub num_lanes: usize,
    pub num_warps: usize,
    pub num_cores: usize,
    pub num_regs: usize,
    pub start_pc: u32,
    pub smem_size: usize,
    #[serde(skip)]
    pub lane_config: LaneConfig,
}

impl Config for MuonConfig {}

impl Default for MuonConfig {
    fn default() -> Self {
        Self {
            num_lanes: 16,
            num_warps: 8,
            num_cores: 1,
            num_regs: 256,
            start_pc: 0x10000000u32,
            smem_size: 0x1_0000, // 64 KiB
            lane_config: LaneConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LaneConfig {
    pub lane_id: usize,
    pub warp_id: usize,
    pub core_id: usize,
    pub cluster_id: usize,
}

impl Default for LaneConfig {
    fn default() -> Self {
        Self {
            lane_id: 0,
            warp_id: 0,
            core_id: 0,
            cluster_id: 0,
        }
    }
}
