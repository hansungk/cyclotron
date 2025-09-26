extern crate lazy_static;

use clap::Parser;
use cyclotron::ui::*;
use std::fs;

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let toml_string = cyclotron::ui::read_toml(&argv.config_path);
    let mut sim = cyclotron::ui::make_sim(&toml_string, Some(argv));
    let result = sim.simulate();
    result
}
