use std::cell::RefCell;
use std::env;
use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::AddAssign;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::muon::gmem::CorePerfSummary;
use crate::timeq::Cycle;

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
    pub icache_stats: crate::timeflow::IcacheStats,
    pub lsu_stats: crate::timeflow::LsuStats,
    pub writeback_stats: crate::timeflow::WritebackStats,
    pub barrier_summary: crate::timeflow::BarrierSummary,
    pub dma_completed: u64,
    pub tensor_completed: u64,
}

impl AddAssign<&CorePerfSummary> for AggregatePerfSummary {
    fn add_assign(&mut self, core: &CorePerfSummary) {
        self.scheduler += &core.scheduler;
        self.smem_util += &core.smem_util;
        self.execute_util += &core.execute_util;
        self.dma_bytes_issued = self.dma_bytes_issued.saturating_add(core.dma_bytes_issued);
        self.dma_bytes_completed = self
            .dma_bytes_completed
            .saturating_add(core.dma_bytes_completed);
        self.dma_util += &core.dma_util;
        self.tensor_bytes_issued = self
            .tensor_bytes_issued
            .saturating_add(core.tensor_bytes_issued);
        self.tensor_bytes_completed = self
            .tensor_bytes_completed
            .saturating_add(core.tensor_bytes_completed);
        self.tensor_util += &core.tensor_util;
        self.stall_summary += &core.stall_summary;
        self.gmem_latency_hist += &core.gmem_latency_hist;
        self.smem_latency_hist += &core.smem_latency_hist;
        self.smem_conflicts += &core.smem_conflicts;
        self.gmem_hits += &core.gmem_hits;
        self.latencies += &core.latencies;
        self.gmem_stats += &core.gmem_stats;
        self.smem_stats += &core.smem_stats;
        self.icache_stats += &core.icache_stats;
        self.lsu_stats += &core.lsu_stats;
        self.writeback_stats += &core.writeback_stats;
        self.barrier_summary += &core.barrier_summary;
        self.dma_completed = self.dma_completed.saturating_add(core.dma_completed);
        self.tensor_completed = self.tensor_completed.saturating_add(core.tensor_completed);
    }
}

#[derive(Debug, Serialize)]
pub struct StatsRecord {
    pub cycle: Cycle,
    pub summary: CorePerfSummary,
}

#[derive(Debug, Serialize)]
pub struct GraphBackpressureRecord {
    pub cycle: Cycle,
    pub edge: String,
    pub src: String,
    pub dst: String,
    pub reason: String,
    pub retry_at: Cycle,
    pub available_at: Option<Cycle>,
    pub capacity: Option<usize>,
    pub size_bytes: u32,
}

pub struct PerfLogSession {
    run_dir: PathBuf,
    stats_writer: RefCell<BufWriter<File>>,
    graph_writer: Option<RefCell<BufWriter<File>>>,
}

unsafe impl Send for PerfLogSession {}
unsafe impl Sync for PerfLogSession {}

impl PerfLogSession {
    pub fn new() -> Option<Self> {
        let run_dir = create_run_dir()?;
        let stats_file = File::create(run_dir.join("stats.jsonl")).ok()?;
        let graph_writer = graph_log_enabled()
            .then(|| {
                File::create(run_dir.join("graph_backpressure.jsonl"))
                    .ok()
                    .map(|file| RefCell::new(BufWriter::new(file)))
            })
            .flatten();

        Some(Self {
            run_dir,
            stats_writer: RefCell::new(BufWriter::new(stats_file)),
            graph_writer,
        })
    }

    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn per_core_path(&self, base: &str, core_id: usize) -> Option<PathBuf> {
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

        let dir = self.run_dir.join(parent);
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        Some(dir.join(file_name))
    }

    pub fn write_stats(&self, record: &StatsRecord) {
        self.write_json_line(&self.stats_writer, record);
    }

    pub fn write_graph_backpressure(&self, record: &GraphBackpressureRecord) {
        if let Some(writer) = &self.graph_writer {
            self.write_json_line(writer, record);
        }
    }

    pub fn write_summary(&self, per_core: Vec<CorePerfSummary>) {
        let summary = RunPerfSummary {
            total: aggregate_summaries(&per_core),
            per_core,
        };
        let path = self.run_dir.join("summary.json");
        if let Ok(payload) = serde_json::to_string_pretty(&summary) {
            let _ = fs::write(path, payload);
        }
    }

    fn write_json_line<T: Serialize>(&self, writer: &RefCell<BufWriter<File>>, record: &T) {
        if let Ok(mut guard) = writer.try_borrow_mut() {
            if let Ok(payload) = serde_json::to_string(record) {
                let _ = writeln!(guard, "{payload}");
            }
        }
    }
}

fn graph_log_enabled() -> bool {
    env::var("CYCLOTRON_GRAPH_LOG")
        .ok()
        .map(|val| {
            let lowered = val.to_ascii_lowercase();
            lowered == "1" || lowered == "true" || lowered == "yes"
        })
        .unwrap_or(false)
}

fn create_run_dir() -> Option<PathBuf> {
    let root = env::var("CYCLOTRON_PERF_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("performance_logs"));
    if fs::create_dir_all(&root).is_err() {
        return None;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let run_dir = root.join(format!("run_{ts}_{pid}"));
    if fs::create_dir_all(&run_dir).is_err() {
        return None;
    }
    Some(run_dir)
}

pub fn aggregate_summaries(per_core: &[CorePerfSummary]) -> AggregatePerfSummary {
    let mut total = AggregatePerfSummary {
        num_cores: per_core.len(),
        ..AggregatePerfSummary::default()
    };
    for core in per_core {
        total += core;
    }
    total
}
