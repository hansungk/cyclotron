use crate::muon::config::MuonConfig;
use crate::neutrino::config::NeutrinoConfig;
use crate::sim::config::{Config, MemConfig, SimConfig};
use crate::sim::top::Sim;
use clap::Parser;
use std::path::{Path, PathBuf};
use toml::Table;

#[derive(Parser, Default)]
#[command(version, about)]
pub struct CyclotronArgs {
    #[arg(help = "Path to config.toml")]
    pub config_path: PathBuf,
    #[arg(long, help = "Override binary path")]
    pub binary_path: Option<PathBuf>,
    #[arg(long, help = "Override number of lanes per warp")]
    pub num_lanes: Option<usize>,
    #[arg(long, help = "Override number of warps per core")]
    pub num_warps: Option<usize>,
    #[arg(long, help = "Override number of cores per cluster")]
    pub num_cores: Option<usize>,
    #[arg(long, help = "Enable log at level (0:none, 1:info, 2:debug)")]
    pub log: Option<u64>,
    #[arg(long, help = "Generate instruction trace")]
    pub gen_trace: Option<bool>,
}

pub fn read_toml(filepath: &Path) -> String {
    std::fs::read_to_string(filepath).unwrap_or_else(|err| {
        eprintln!(
            "cyclotron: failed to read config file at {}: {}",
            filepath.display(),
            err
        );
        std::process::exit(1);
    })
}

fn maybe_get<'a>(table: &'a Option<Table>, key: &str) -> Option<&'a toml::Value> {
    table.as_ref().and_then(|t| t.get(key))
}

/// Make a Sim object.
/// If `toml_string` is given, override default configs with TOML values.
/// If `cli_args` is given, override post-TOML configs with CLI arguments.
pub fn make_sim(toml_string: Option<&str>, cli_args: &Option<CyclotronArgs>) -> Sim {
    let config_table = toml_string.map(|s| toml::from_str(s).expect("cannot parse config toml"));
    let mut sim_config = SimConfig::from_section(maybe_get(&config_table, "sim"));
    let mem_config = MemConfig::from_section(maybe_get(&config_table, "mem"));
    let mut muon_config = MuonConfig::from_section(maybe_get(&config_table, "muon"));
    let mut neutrino_config = NeutrinoConfig::from_section(maybe_get(&config_table,"neutrino"));

    if let Some(args) = cli_args {
        sim_config.elf = args.binary_path.as_ref().cloned().unwrap_or(sim_config.elf);
        sim_config.log_level = args.log.unwrap_or(sim_config.log_level);
        sim_config.trace = args.gen_trace.unwrap_or(sim_config.trace);
        muon_config.num_lanes = args.num_lanes.unwrap_or(muon_config.num_lanes);
        muon_config.num_warps = args.num_warps.unwrap_or(muon_config.num_warps);
        muon_config.num_cores = args.num_cores.unwrap_or(muon_config.num_cores);
    }

    neutrino_config.muon_config = muon_config.clone();

    Sim::new(sim_config, muon_config, neutrino_config, mem_config)
}
