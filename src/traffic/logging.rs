#[derive(Debug, Default, Clone)]
pub struct PatternCheckpoint {
    pub core_id: usize,
    pub pattern_name: String,
    pub finished_cycle: u64,
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
}
