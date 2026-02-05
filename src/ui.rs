use crate::muon::config::MuonConfig;
use crate::neutrino::config::NeutrinoConfig;
use crate::sim::config::{Config, MemConfig, SimConfig};
use crate::sim::top::Sim;
use crate::timeflow::CoreGraphConfig;
use clap::Parser;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Parser)]
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
        eprintln!("failed to read config file {}: {}", filepath.display(), err);
        std::process::exit(1);
    })
}

fn merge_values(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Table(dst_table), Value::Table(src_table)) => {
            for (key, value) in src_table {
                match dst_table.get_mut(&key) {
                    Some(existing) => merge_values(existing, value),
                    None => {
                        dst_table.insert(key, value);
                    }
                }
            }
        }
        (dst_value, src_value) => {
            *dst_value = src_value;
        }
    }
}

fn load_timing_config(config_path: Option<&Path>, config_table: &Table) -> CoreGraphConfig {
    let base_dir = config_path
        .and_then(|path| path.parent())
        .unwrap_or_else(|| Path::new("."));
    let mut merged = Value::Table(Table::new());

    if let Some(Value::Table(timing_table)) = config_table.get("timing") {
        if let Some(Value::Array(includes)) = timing_table.get("include") {
            for entry in includes {
                let path = match entry {
                    Value::String(path) => path,
                    _ => continue,
                };
                let include_path = base_dir.join(path);
                let toml_string = read_toml(&include_path);
                let value: Value = toml::from_str(&toml_string).unwrap_or_else(|err| {
                    eprintln!(
                        "failed to parse timing config {}: {}",
                        include_path.display(),
                        err
                    );
                    std::process::exit(1);
                });
                merge_values(&mut merged, value);
            }
        }

        let mut inline = timing_table.clone();
        inline.remove("include");
        if !inline.is_empty() {
            merge_values(&mut merged, Value::Table(inline));
        }
    }

    merged.try_into().unwrap_or_default()
}

/// Make a Sim object from the TOML configuration.
/// If `cli_args` is given, override TOML options with CLI arguments.
pub fn make_sim(
    toml_string: &str,
    cli_args: Option<CyclotronArgs>,
    config_path: Option<&Path>,
) -> Sim {
    let config_table: Table = toml::from_str(toml_string).expect("cannot parse config toml");
    let mut sim_config = SimConfig::from_section(config_table.get("sim"));
    let mem_config = MemConfig::from_section(config_table.get("mem"));
    let mut muon_config = MuonConfig::from_section(config_table.get("muon"));
    let mut neutrino_config = NeutrinoConfig::from_section(config_table.get("neutrino"));
    let timing_config = load_timing_config(config_path, &config_table);

    // override toml configs with CLI args
    if let Some(args) = cli_args {
        sim_config.elf = args.binary_path.unwrap_or(sim_config.elf);
        sim_config.log_level = args.log.unwrap_or(sim_config.log_level);
        sim_config.trace = args.gen_trace.unwrap_or(sim_config.trace);
        muon_config.num_lanes = args.num_lanes.unwrap_or(muon_config.num_lanes);
        muon_config.num_warps = args.num_warps.unwrap_or(muon_config.num_warps);
        muon_config.num_cores = args.num_cores.unwrap_or(muon_config.num_cores);
    }

    neutrino_config.muon_config = muon_config.clone();

    Sim::new(
        sim_config,
        muon_config,
        neutrino_config,
        mem_config,
        timing_config,
    )
}
