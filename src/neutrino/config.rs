use serde::Deserialize;
use crate::muon::config::MuonConfig;
use crate::sim::config::Config;

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct NeutrinoConfig {
    #[serde(default)]
    pub num_entries: usize,
    #[serde(default)]
    pub task_id_width: usize,
    #[serde(default)]
    pub counter_width: usize,
    #[serde(default)]
    pub in_order_issue: bool,
    #[serde(skip)]
    pub muon_config: MuonConfig,
}

impl Config for NeutrinoConfig {}

impl Default for NeutrinoConfig {
    fn default() -> Self {
        Self {
            num_entries: 32,
            task_id_width: 8,
            counter_width: 24,
            in_order_issue: false,
            muon_config: MuonConfig::default(),
        }
    }
}
