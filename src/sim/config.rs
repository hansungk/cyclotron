use std::path::PathBuf;
use std::str::FromStr;

use log::warn;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use toml::*;

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FrontendMode {
    #[default]
    Elf,
    TrafficSmem,
}

impl FromStr for FrontendMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "elf" => Ok(Self::Elf),
            "traffic_smem" => Ok(Self::TrafficSmem),
            _ => Err(format!(
                "unsupported frontend mode '{}', expected one of: elf, traffic_smem",
                value
            )),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SimConfig {
    pub elf: PathBuf,
    pub log_level: u64,
    pub timeout: u64,
    pub trace: bool,
    pub timing: bool,
    pub frontend_mode: FrontendMode,
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
            elf: PathBuf::new(),
            log_level: 0,
            timeout: 10000000,
            trace: false,
            timing: false,
            frontend_mode: FrontendMode::Elf,
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
