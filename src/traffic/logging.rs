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
    fn sanitize_token(s: &str) -> String {
        s.replace(' ', "_")
    }

    fn op_from_pattern(pattern_name: &str) -> &'static str {
        if pattern_name.ends_with("_w") {
            "w"
        } else if pattern_name.ends_with("_r") {
            "r"
        } else {
            "na"
        }
    }

    pub fn log_pattern_checkpoint(
        domain: &str,
        core_id: usize,
        pattern_name: &str,
        cycle: u64,
        duration: u64,
    ) {
        Self::log_pattern_checkpoint_with_metadata(
            domain,
            "traffic_frontend",
            core_id,
            pattern_name,
            cycle,
            duration,
            0,
            0,
            0,
            0,
            0,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_pattern_checkpoint_with_metadata(
        domain: &str,
        suite: &str,
        core_id: usize,
        pattern_name: &str,
        cycle: u64,
        duration: u64,
        reqs_per_lane: u32,
        active_lanes: usize,
        issue_gap: u64,
        max_outstanding: usize,
        working_set: u64,
    ) {
        let pattern = Self::sanitize_token(pattern_name);
        let op = Self::op_from_pattern(pattern_name);
        let suite = Self::sanitize_token(suite);
        println!(
            "[STIM] domain={} suite={} phase=traffic pattern={} op={} cycle={} duration={} reqs_per_lane={} active_lanes={} issue_gap={} max_outstanding={} working_set={} wait_cycles=0 note=core_{}",
            domain,
            suite,
            pattern,
            op,
            cycle,
            duration,
            reqs_per_lane,
            active_lanes,
            issue_gap,
            max_outstanding,
            working_set,
            core_id
        );
    }

    pub fn log_core_done(domain: &str, core_id: usize, cycle: u64) {
        println!(
            "[STIM] domain={} suite=traffic_frontend phase=report pattern=none op=na cycle={} duration=0 reqs_per_lane=0 active_lanes=0 issue_gap=0 max_outstanding=0 working_set=0 wait_cycles=0 note=core_{}_done",
            domain, cycle, core_id
        );
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
