use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use cyclotron::base::behavior::*;
use cyclotron::muon::config::MuonConfig;
use cyclotron::sim::top::{CyclotronTop, CyclotronTopConfig};

#[derive(Parser)]
#[command(version, about)]
struct CyclotronArgs {
    binary_path: PathBuf,

    #[arg(long)]
    num_lanes: Option<usize>,
    #[arg(long)]
    num_warps: Option<usize>,
    #[arg(long)]
    num_cores: Option<usize>,
}

pub fn main() {
    env_logger::init();
    let argv = CyclotronArgs::parse();
    let mut muon_config = MuonConfig {
        num_lanes: 4,
        num_warps: 4,
        num_cores: 1,
        lane_config: Default::default(),
    };

    muon_config.num_lanes = argv.num_lanes.unwrap_or(muon_config.num_lanes);
    muon_config.num_warps = argv.num_warps.unwrap_or(muon_config.num_warps);
    muon_config.num_cores = argv.num_cores.unwrap_or(muon_config.num_cores);
    
    let mut top = CyclotronTop::new(Arc::new(CyclotronTopConfig {
        timeout: 50000,
        elf_path: argv.binary_path.clone(),
        muon_config
    }));

    top.muon.reset();
    for _ in 0..top.timeout {
        top.tick_one()
    }
}
