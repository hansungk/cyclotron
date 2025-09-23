extern crate lazy_static;

use std::fs;
use toml::Table;
use std::path::PathBuf;
use clap::Parser;
use cyclotron::muon::config::MuonConfig;
use cyclotron::neutrino::config::NeutrinoConfig;
use cyclotron::sim::config::{Config, MemConfig, SimConfig};
use cyclotron::sim::top::Sim;

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
    #[arg(long, help="Enable log at level (0:none, 1:info, 2:debug)")]
    log: Option<u64>,
    #[arg(long, help="Generate instruction trace")]
    gen_trace: Option<bool>,
}

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let config = fs::read_to_string(&argv.config_path).unwrap_or_else(|err| {
        eprintln!("failed to read config file: {}", err);
        std::process::exit(1);
    });

    let config_table: Table = toml::from_str(&config).expect("cannot parse config toml");
    let mut sim_config = SimConfig::from_section(config_table.get("sim"));
    let mem_config = MemConfig::from_section(config_table.get("mem"));
    let mut muon_config = MuonConfig::from_section(config_table.get("muon"));
    let mut neutrino_config = NeutrinoConfig::from_section(config_table.get("neutrino"));

    // override toml configs with argv
    sim_config.log_level = argv.log.unwrap_or(sim_config.log_level);
    sim_config.trace = argv.gen_trace.unwrap_or(sim_config.trace);
    muon_config.num_lanes = argv.num_lanes.unwrap_or(muon_config.num_lanes);
    muon_config.num_warps = argv.num_warps.unwrap_or(muon_config.num_warps);
    muon_config.num_cores = argv.num_cores.unwrap_or(muon_config.num_cores);
    neutrino_config.muon_config = muon_config.clone();

    let mut sim = Sim::new(sim_config, muon_config, neutrino_config, mem_config);
    let result = sim.simulate();
    result
}
