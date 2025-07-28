extern crate lazy_static;

use std::fs;
use std::sync::Arc;
use toml::Table;
use std::path::PathBuf;
use clap::Parser;
use cyclotron::base::behavior::*;
use cyclotron::base::module::IsModule;
use cyclotron::muon::config::MuonConfig;
use cyclotron::neutrino::config::NeutrinoConfig;
use cyclotron::sim::config::{Config, MemConfig, SimConfig};
use cyclotron::sim::top::{ClusterConfig, CyclotronTop, CyclotronTopConfig};

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

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let config = fs::read_to_string(&argv.config_path).unwrap_or_else(|err| {
        eprintln!("failed to read config file: {}", err);
        std::process::exit(1);
    });

    let config_table: Table = toml::from_str(&config).expect("cannot parse config toml");
    let sim_config = SimConfig::from_section(config_table.get("sim"));
    let mem_config = MemConfig::from_section(config_table.get("mem"));
    let mut muon_config = MuonConfig::from_section(config_table.get("muon"));
    let mut neutrino_config = NeutrinoConfig::from_section(config_table.get("neutrino"));

    // optionally override config.toml
    muon_config.num_lanes = argv.num_lanes.unwrap_or(muon_config.num_lanes);
    muon_config.num_warps = argv.num_warps.unwrap_or(muon_config.num_warps);
    muon_config.num_cores = argv.num_cores.unwrap_or(muon_config.num_cores);
    neutrino_config.muon_config = muon_config.clone();

    let mut top = CyclotronTop::new(Arc::new(CyclotronTopConfig {
        timeout: sim_config.timeout,
        elf_path: argv.binary_path.unwrap_or(sim_config.elf.into()),
        cluster_config: ClusterConfig {
            muon_config,
            neutrino_config,
        },
        mem_config,
    }));

    top.reset();
    for _ in 0..top.timeout {
        top.tick_one();
        if top.finished() {
            println!("simulation has finished");
            if let Some(mut tohost) = top.clusters[0].cores[0].scheduler.state().tohost {
                if tohost > 0 {
                    tohost >>= 1;
                    println!("failed test case {}", tohost);
                    return Err(tohost)
                } else {
                    println!("test passed");
                }
            }
            return Ok(());
        }
    }
    println!("timeout after {} cycles", top.timeout);
    Err(0)
}
