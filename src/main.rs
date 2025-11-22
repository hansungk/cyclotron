extern crate lazy_static;

use clap::Parser;
use cyclotron::ui::*;

pub fn main() -> Result<(), u32> {
    env_logger::init();

    let argv = CyclotronArgs::parse();
    let toml_string = read_toml(argv.config_path.as_path());
    let mut sim = make_sim(&toml_string, &Some(argv));
    let result = sim.simulate();
    result
}
