use serde::Deserialize;

use crate::sim::config::Config;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TrafficConfig {
    pub enabled: bool,
    pub file: Option<String>,
    pub lockstep_patterns: bool,
    pub reqs_per_pattern: u32,
    pub num_lanes: usize,
    pub address: TrafficAddressConfig,
    pub issue: TrafficIssueConfig,
    pub logging: TrafficLoggingConfig,
    pub patterns: Vec<TrafficPatternSpec>,
}

impl Config for TrafficConfig {}

impl Default for TrafficConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            file: None,
            lockstep_patterns: true,
            reqs_per_pattern: 4096,
            num_lanes: 16,
            address: TrafficAddressConfig::default(),
            issue: TrafficIssueConfig::default(),
            logging: TrafficLoggingConfig::default(),
            patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TrafficAddressConfig {
    pub cluster_id: usize,
    pub smem_base: u64,
    pub smem_size_bytes: u64,
}

impl Default for TrafficAddressConfig {
    fn default() -> Self {
        Self {
            cluster_id: 0,
            smem_base: 0x4000_0000,
            smem_size_bytes: 128 << 10,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TrafficIssueConfig {
    pub max_inflight_per_lane: usize,
    pub retry_backoff_min: u64,
}

impl Default for TrafficIssueConfig {
    fn default() -> Self {
        Self {
            max_inflight_per_lane: 16,
            retry_backoff_min: 1,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TrafficLoggingConfig {
    pub print_traffic_lines: bool,
    pub results_json: Option<String>,
    pub results_csv: Option<String>,
}

impl Default for TrafficLoggingConfig {
    fn default() -> Self {
        Self {
            print_traffic_lines: true,
            results_json: None,
            results_csv: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TrafficPatternSpec {
    pub name: String,
    pub kind: String,
    pub req_bytes: u32,
    pub op: String,
    pub warp_stride: u32,
    pub lane_stride: u32,
    pub tile_m: u32,
    pub tile_n: u32,
    pub tile_size: u32,
    pub transpose: bool,
    pub random_min: u32,
    pub random_max: u32,
    pub seed: u64,
    pub within_bytes: Option<u64>,
}

impl Default for TrafficPatternSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: String::new(),
            req_bytes: 4,
            op: "read".to_string(),
            warp_stride: 1,
            lane_stride: 1,
            tile_m: 16,
            tile_n: 16,
            tile_size: 16,
            transpose: false,
            random_min: 0,
            random_max: 0,
            seed: 0,
            within_bytes: None,
        }
    }
}
