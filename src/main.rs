extern crate lazy_static;

use std::fs;
use std::sync::Arc;
use toml::Table;
use std::path::PathBuf;
use clap::Parser;
use cyclotron::base::behavior::*;
use cyclotron::muon::config::MuonConfig;
use cyclotron::sim::config::{Config, SimConfig};
use cyclotron::sim::top::{CyclotronTop, CyclotronTopConfig};

#[derive(Parser)]
#[command(version, about)]
struct CyclotronArgs {
    #[arg(help="Path to config.toml")]
    config_path: PathBuf,
    #[arg(long, help="Override binary path")]
    binary_path: Option<PathBuf>,
    #[arg(long, help="Override number of lanes per warp")]
    num_lanes: Option<usize>,
    #[arg(long, help="Override number of warps per core")]
    num_warps: Option<usize>,
    #[arg(long, help="Override number of cores per cluster")]
    num_cores: Option<usize>,
}

pub fn main() {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let config = fs::read_to_string(&argv.config_path).unwrap_or_else(|err| {
        eprintln!("failed to read config file: {}", err);
        std::process::exit(1);
    });

    let config_table: Table = toml::from_str(&config).expect("cannot parse config toml");
    let sim_config = SimConfig::from_section(config_table.get("sim"));
    let mut muon_config = MuonConfig::from_section(config_table.get("muon"));

    // optionally override config.toml
    muon_config.num_lanes = argv.num_lanes.unwrap_or(muon_config.num_lanes);
    muon_config.num_warps = argv.num_warps.unwrap_or(muon_config.num_warps);
    muon_config.num_cores = argv.num_cores.unwrap_or(muon_config.num_cores);

    let mut top = CyclotronTop::new(Arc::new(CyclotronTopConfig {
        timeout: sim_config.timeout,
        elf_path: argv.binary_path.unwrap_or(sim_config.elf.into()),
        muon_config,
    }));

    top.reset();
    for _ in 0..top.timeout {
        top.tick_one();
        if top.finished() {
            println!("simulation has finished");
            return;
        }
    }
    println!("timeout after {} cycles", top.timeout);
}
