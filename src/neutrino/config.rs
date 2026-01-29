use crate::muon::config::MuonConfig;
use crate::sim::config::Config;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(default)]
pub struct NeutrinoConfig {
    pub num_entries: usize,
    pub task_id_width: usize,
    pub counter_width: usize,
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
