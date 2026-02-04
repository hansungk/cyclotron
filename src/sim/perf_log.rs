use std::env;
use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::muon::gmem::CorePerfSummary;
use crate::timeq::Cycle;

static PERF_RUN_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn perf_run_dir() -> Option<PathBuf> {
    if let Some(path) = PERF_RUN_DIR.get() {
        return Some(path.clone());
    }

    let root = env::var("CYCLOTRON_PERF_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("performance_logs"));
    if fs::create_dir_all(&root).is_err() {
        return None;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    let run_dir = root.join(format!("run_{ts}_{pid}"));
    if fs::create_dir_all(&run_dir).is_err() {
        return None;
    }

    let _ = PERF_RUN_DIR.set(run_dir.clone());
    Some(run_dir)
}

pub fn per_core_path(base: &str, core_id: usize) -> Option<PathBuf> {
    let base_path = PathBuf::from(base);
    let file_name = match base_path.file_name().and_then(|s| s.to_str()) {
        Some(name) => name.to_string(),
        None => return None,
    };
    let file_name = core_file_name(&file_name, core_id);

    let parent = base_path.parent().unwrap_or_else(|| Path::new(""));
    if base_path.is_absolute() {
        let dir = parent.to_path_buf();
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        return Some(dir.join(file_name));
    }

    let run_dir = perf_run_dir()?;
    let dir = run_dir.join(parent);
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(dir.join(file_name))
}

fn core_file_name(base: &str, core_id: usize) -> String {
    let path = Path::new(base);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(base);
    let ext = path.extension().and_then(|s| s.to_str());
    match ext {
        Some(ext) => format!("{stem}_core{core_id}.{ext}"),
        None => format!("{stem}_core{core_id}"),
    }
}

#[derive(Debug, Serialize)]
pub struct RunPerfSummary {
    pub per_core: Vec<CorePerfSummary>,
    pub total: AggregatePerfSummary,
}

#[derive(Debug, Default, Serialize)]
pub struct AggregatePerfSummary {
    pub num_cores: usize,
    pub scheduler: crate::muon::gmem::SchedulerSummary,
    pub smem_util: crate::muon::gmem::SmemUtilSummary,
    pub execute_util: crate::muon::gmem::ExecuteUtilSummary,
    pub dma_bytes_issued: u64,
    pub dma_bytes_completed: u64,
    pub dma_util: crate::muon::gmem::BasicUtilSummary,
    pub tensor_bytes_issued: u64,
    pub tensor_bytes_completed: u64,
    pub tensor_util: crate::muon::gmem::BasicUtilSummary,
    pub stall_summary: crate::muon::gmem::StallSummary,
    pub gmem_latency_hist: crate::muon::gmem::LatencyHistogram,
    pub smem_latency_hist: crate::muon::gmem::LatencyHistogram,
    pub smem_conflicts: crate::muon::gmem::SmemConflictSummary,
    pub gmem_hits: crate::muon::gmem::GmemHitSummary,
    pub latencies: crate::muon::gmem::LatencySummary,
    pub gmem_stats: crate::timeflow::GmemStats,
    pub smem_stats: crate::timeflow::SmemStats,
    pub dma_completed: u64,
    pub tensor_completed: u64,
}

#[derive(Debug, Serialize)]
pub struct StatsRecord {
    pub cycle: Cycle,
    pub summary: CorePerfSummary,
}

pub struct StatsLog {
    writer: Mutex<BufWriter<File>>,
}

impl StatsLog {
    pub(crate) fn write(&self, record: &StatsRecord) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Ok(payload) = serde_json::to_string(record) {
                let _ = writeln!(guard, "{payload}");
            }
        }
    }
}

static STATS_LOGGER: OnceLock<Option<Arc<StatsLog>>> = OnceLock::new();

fn create_stats_logger() -> Option<Arc<StatsLog>> {
    perf_run_dir().and_then(|run_dir| {
        let path = run_dir.join("stats.jsonl");
        File::create(path).ok().map(|file| {
            Arc::new(StatsLog {
                writer: Mutex::new(BufWriter::new(file)),
            })
        })
    })
}

pub fn stats_logger() -> Option<Arc<StatsLog>> {
    STATS_LOGGER.get_or_init(|| create_stats_logger()).clone()
}

pub fn aggregate_summaries(per_core: &[CorePerfSummary]) -> AggregatePerfSummary {
    let mut total = AggregatePerfSummary::default();
    total.num_cores = per_core.len();

    for (idx, core) in per_core.iter().enumerate() {
        if idx == 0 {
            total.scheduler.issue_width = core.scheduler.issue_width;
        }
        total.scheduler.cycles = total.scheduler.cycles.saturating_add(core.scheduler.cycles);
        total.scheduler.active_warps_sum = total
            .scheduler
            .active_warps_sum
            .saturating_add(core.scheduler.active_warps_sum);
        total.scheduler.eligible_warps_sum = total
            .scheduler
            .eligible_warps_sum
            .saturating_add(core.scheduler.eligible_warps_sum);
        total.scheduler.issued_warps_sum = total
            .scheduler
            .issued_warps_sum
            .saturating_add(core.scheduler.issued_warps_sum);

        total.smem_util.cycles = total.smem_util.cycles.saturating_add(core.smem_util.cycles);
        total.smem_util.lane_busy_sum = total
            .smem_util
            .lane_busy_sum
            .saturating_add(core.smem_util.lane_busy_sum);
        total.smem_util.bank_busy_sum = total
            .smem_util
            .bank_busy_sum
            .saturating_add(core.smem_util.bank_busy_sum);
        total.smem_util.lane_total = total.smem_util.lane_total.max(core.smem_util.lane_total);
        total.smem_util.bank_total = total.smem_util.bank_total.max(core.smem_util.bank_total);

        total.execute_util.cycles = total
            .execute_util
            .cycles
            .saturating_add(core.execute_util.cycles);
        total.execute_util.int_busy_sum = total
            .execute_util
            .int_busy_sum
            .saturating_add(core.execute_util.int_busy_sum);
        total.execute_util.int_mul_busy_sum = total
            .execute_util
            .int_mul_busy_sum
            .saturating_add(core.execute_util.int_mul_busy_sum);
        total.execute_util.int_div_busy_sum = total
            .execute_util
            .int_div_busy_sum
            .saturating_add(core.execute_util.int_div_busy_sum);
        total.execute_util.fp_busy_sum = total
            .execute_util
            .fp_busy_sum
            .saturating_add(core.execute_util.fp_busy_sum);
        total.execute_util.sfu_busy_sum = total
            .execute_util
            .sfu_busy_sum
            .saturating_add(core.execute_util.sfu_busy_sum);

        total.dma_bytes_issued = total.dma_bytes_issued.saturating_add(core.dma_bytes_issued);
        total.dma_bytes_completed = total
            .dma_bytes_completed
            .saturating_add(core.dma_bytes_completed);
        total.dma_util.cycles = total.dma_util.cycles.saturating_add(core.dma_util.cycles);
        total.dma_util.busy_sum = total
            .dma_util
            .busy_sum
            .saturating_add(core.dma_util.busy_sum);

        total.tensor_bytes_issued = total
            .tensor_bytes_issued
            .saturating_add(core.tensor_bytes_issued);
        total.tensor_bytes_completed = total
            .tensor_bytes_completed
            .saturating_add(core.tensor_bytes_completed);
        total.tensor_util.cycles = total
            .tensor_util
            .cycles
            .saturating_add(core.tensor_util.cycles);
        total.tensor_util.busy_sum = total
            .tensor_util
            .busy_sum
            .saturating_add(core.tensor_util.busy_sum);

        total.stall_summary.gmem_queue_full = total
            .stall_summary
            .gmem_queue_full
            .saturating_add(core.stall_summary.gmem_queue_full);
        total.stall_summary.gmem_busy = total
            .stall_summary
            .gmem_busy
            .saturating_add(core.stall_summary.gmem_busy);
        total.stall_summary.smem_queue_full = total
            .stall_summary
            .smem_queue_full
            .saturating_add(core.stall_summary.smem_queue_full);
        total.stall_summary.smem_busy = total
            .stall_summary
            .smem_busy
            .saturating_add(core.stall_summary.smem_busy);

        total.gmem_latency_hist.accumulate(&core.gmem_latency_hist);
        total.smem_latency_hist.accumulate(&core.smem_latency_hist);

        total.smem_conflicts.instructions = total
            .smem_conflicts
            .instructions
            .saturating_add(core.smem_conflicts.instructions);
        total.smem_conflicts.active_lanes = total
            .smem_conflicts
            .active_lanes
            .saturating_add(core.smem_conflicts.active_lanes);
        total.smem_conflicts.conflict_lanes = total
            .smem_conflicts
            .conflict_lanes
            .saturating_add(core.smem_conflicts.conflict_lanes);
        total.smem_conflicts.unique_banks = total
            .smem_conflicts
            .unique_banks
            .saturating_add(core.smem_conflicts.unique_banks);
        total.smem_conflicts.unique_subbanks = total
            .smem_conflicts
            .unique_subbanks
            .saturating_add(core.smem_conflicts.unique_subbanks);

        total.gmem_hits.l0_accesses = total
            .gmem_hits
            .l0_accesses
            .saturating_add(core.gmem_hits.l0_accesses);
        total.gmem_hits.l0_hits = total
            .gmem_hits
            .l0_hits
            .saturating_add(core.gmem_hits.l0_hits);
        total.gmem_hits.l1_accesses = total
            .gmem_hits
            .l1_accesses
            .saturating_add(core.gmem_hits.l1_accesses);
        total.gmem_hits.l1_hits = total
            .gmem_hits
            .l1_hits
            .saturating_add(core.gmem_hits.l1_hits);
        total.gmem_hits.l2_accesses = total
            .gmem_hits
            .l2_accesses
            .saturating_add(core.gmem_hits.l2_accesses);
        total.gmem_hits.l2_hits = total
            .gmem_hits
            .l2_hits
            .saturating_add(core.gmem_hits.l2_hits);

        total.latencies.gmem_count = total
            .latencies
            .gmem_count
            .saturating_add(core.latencies.gmem_count);
        total.latencies.gmem_sum = total
            .latencies
            .gmem_sum
            .saturating_add(core.latencies.gmem_sum);
        total.latencies.smem_count = total
            .latencies
            .smem_count
            .saturating_add(core.latencies.smem_count);
        total.latencies.smem_sum = total
            .latencies
            .smem_sum
            .saturating_add(core.latencies.smem_sum);

        total.gmem_stats += &core.gmem_stats;

        total.smem_stats.issued = total
            .smem_stats
            .issued
            .saturating_add(core.smem_stats.issued);
        total.smem_stats.completed = total
            .smem_stats
            .completed
            .saturating_add(core.smem_stats.completed);
        total.smem_stats.queue_full_rejects = total
            .smem_stats
            .queue_full_rejects
            .saturating_add(core.smem_stats.queue_full_rejects);
        total.smem_stats.busy_rejects = total
            .smem_stats
            .busy_rejects
            .saturating_add(core.smem_stats.busy_rejects);
        total.smem_stats.bytes_issued = total
            .smem_stats
            .bytes_issued
            .saturating_add(core.smem_stats.bytes_issued);
        total.smem_stats.bytes_completed = total
            .smem_stats
            .bytes_completed
            .saturating_add(core.smem_stats.bytes_completed);
        total.smem_stats.max_inflight = total
            .smem_stats
            .max_inflight
            .max(core.smem_stats.max_inflight);
        total.smem_stats.max_completion_queue = total
            .smem_stats
            .max_completion_queue
            .max(core.smem_stats.max_completion_queue);
        total.smem_stats.last_completion_cycle = match (
            total.smem_stats.last_completion_cycle,
            core.smem_stats.last_completion_cycle,
        ) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, Some(b)) => Some(b),
            (a, None) => a,
        };

        total.dma_completed = total.dma_completed.saturating_add(core.dma_completed);
        total.tensor_completed = total.tensor_completed.saturating_add(core.tensor_completed);
    }

    total
}

pub fn write_summary(per_core: Vec<CorePerfSummary>) {
    let run_dir = match perf_run_dir() {
        Some(dir) => dir,
        None => return,
    };
    let summary = RunPerfSummary {
        total: aggregate_summaries(&per_core),
        per_core,
    };
    let path = run_dir.join("summary.json");
    if let Ok(payload) = serde_json::to_string_pretty(&summary) {
        let _ = fs::write(path, payload);
    }
}
