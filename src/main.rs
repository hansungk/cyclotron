extern crate lazy_static;

use std::error::Error;
use std::sync::Arc;
use cyclotron::base::behavior::*;
use cyclotron::muon::config::MuonConfig;
use cyclotron::sim::top::{CyclotronTop, CyclotronTopConfig};

pub fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let mut top = CyclotronTop::new(Arc::new(CyclotronTopConfig {
        timeout: 50000,
        elf_path: "test/gemm.elf".into(),
        muon_config: MuonConfig {
            num_lanes: 4,
            num_warps: 4,
            num_cores: 1,
            lane_config: Default::default(),
        },
    }));

    top.muon.reset();
    for _ in 0..top.timeout {
        top.tick_one()
    }
    Ok(())
}
