use log::warn;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use toml::*;

#[derive(Debug, Deserialize, Clone)]
pub struct SimConfig {
    #[serde(default)]
    pub elf: String,
    #[serde(default)]
    pub log_level: String,
    #[serde(default)]
    pub timeout: u64,
    #[serde(default)]
    pub benchmark: usize,
}

pub trait Config: DeserializeOwned + Default {
    fn from_section(section: Option<&Value>) -> Self {
        match section {
            Some(value) => value.clone().try_into().expect("cannot deserialize config"),
            None => {
                warn!("config section not found");
                Self::default()
            }
        }
    }
}

impl Config for SimConfig {}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            elf: "".to_string(),
            log_level: "warn".to_string(),
            timeout: 10000,
            benchmark: 0,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct MemConfig {
    pub io_cout_addr: usize,
    pub io_cout_size: usize,
}

impl Config for MemConfig {}

impl Default for MemConfig {
    fn default() -> Self {
        Self {
            io_cout_addr: 0xFF080000,
            io_cout_size: 64,
        }
    }
}
