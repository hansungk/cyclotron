extern crate lazy_static;

use std::env::args;
use std::fs;
use std::sync::Arc;
use toml::Table;
use cyclotron::base::behavior::*;
use cyclotron::base::component::IsComponent;
use cyclotron::muon::config::MuonConfig;
use cyclotron::sim::config::{Config, SimConfig};
use cyclotron::sim::top::{CyclotronTop, CyclotronTopConfig};

pub fn main() {
    env_logger::init();
    let argv: Vec<_> = args().collect();
    if argv.len() < 2 {
        println!("usage: {} <config>", argv[0]);
        return;
    }

    let config = fs::read_to_string(&argv[1].clone()).unwrap_or_else(|err| {
        eprintln!("failed to read config file: {}", err);
        std::process::exit(1);
    });

    let config_table: Table = toml::from_str(&config).expect(&"cannot parse config toml");
    let sim_config = SimConfig::from_section(config_table.get("sim"));
    println!("elf is {}", config_table.get("sim").unwrap());
    let muon_config = MuonConfig::from_section(config_table.get("muon"));

    let mut top = CyclotronTop::new(Arc::new(CyclotronTopConfig {
        timeout: sim_config.timeout,
        elf_path: sim_config.elf,
        muon_config,
    }));

    top.muon.reset();
    for _ in 0..top.timeout {
        top.tick_one();
        if top.muon.scheduler.base().state.active_warps == 0 {
            return;
        }
    }
}
