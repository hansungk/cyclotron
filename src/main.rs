extern crate lazy_static;

use clap::Parser;
use cyclotron::ui::*;

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let config_path = argv.config_path.clone();
    let toml_string = read_toml(config_path.as_path());
    let mut sim = make_sim(&toml_string, Some(argv), Some(config_path.as_path()));
    let result = sim.simulate();
    result
}
