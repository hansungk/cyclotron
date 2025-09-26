extern crate lazy_static;

use clap::Parser;
use cyclotron::ui::*;
use std::fs;

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let toml_string = fs::read_to_string(&argv.config_path).unwrap_or_else(|err| {
        eprintln!("failed to read config file: {}", err);
        std::process::exit(1);
    });

    let mut sim = cyclotron::ui::make_sim(&toml_string, Some(argv));
    let result = sim.simulate();
    result
}
