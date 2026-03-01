use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize)]
pub struct PatternCheckpoint {
    pub core_id: usize,
    pub pattern_idx: usize,
    pub pattern_name: String,
    pub finished_cycle: u64,
    pub duration_cycles: u64,
}

pub struct TrafficLogger;

impl TrafficLogger {
    pub fn log_pattern_checkpoint(core_id: usize, pattern_name: &str, cycle: u64) {
        println!(
            "[TRAFFIC] core {} {} finished at time {:>10}",
            core_id, pattern_name, cycle
        );
    }

    pub fn log_core_done(core_id: usize) {
        println!("[TRAFFIC] core {} all done!", core_id);
    }

    pub fn write_json(
        path: &str,
        checkpoints: &[PatternCheckpoint],
        metadata: Value,
    ) -> io::Result<()> {
        let p = Path::new(path);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let mut last_cycle = 0u64;
        for cp in checkpoints {
            last_cycle = last_cycle.max(cp.finished_cycle);
        }

        let payload = json!({
            "metadata": metadata,
            "summary": {
                "checkpoint_count": checkpoints.len(),
                "last_cycle": last_cycle,
            },
            "checkpoints": checkpoints,
        });
        let data = serde_json::to_string_pretty(&payload)?;
        fs::write(p, data.as_bytes())
    }

    pub fn write_csv(path: &str, checkpoints: &[PatternCheckpoint]) -> io::Result<()> {
        let p = Path::new(path);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let mut file = fs::File::create(p)?;
        writeln!(
            file,
            "core_id,pattern_idx,pattern_name,finished_cycle,duration_cycles"
        )?;
        for cp in checkpoints {
            let escaped = cp.pattern_name.replace('"', "\"\"");
            writeln!(
                file,
                "{},{},\"{}\",{},{}",
                cp.core_id, cp.pattern_idx, escaped, cp.finished_cycle, cp.duration_cycles
            )?;
        }
        Ok(())
    }
}
