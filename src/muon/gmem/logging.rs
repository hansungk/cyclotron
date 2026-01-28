use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::timeq::Cycle;

pub(crate) struct TraceSink {
    writer: BufWriter<File>,
    wrote_header: bool,
}

impl TraceSink {
    pub(crate) fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            wrote_header: false,
        })
    }

    pub(crate) fn write_event(
        &mut self,
        cycle: Cycle,
        event: &str,
        warp: usize,
        request_id: Option<u64>,
        bytes: u32,
        reason: Option<&str>,
    ) {
        if !self.wrote_header {
            let _ = writeln!(self.writer, "cycle,event,warp,request_id,bytes,reason");
            self.wrote_header = true;
        }

        let req_id_str = request_id
            .map(|id| id.to_string())
            .unwrap_or_else(String::new);
        let reason_str = reason.unwrap_or("");
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{},{}",
            cycle, event, warp, req_id_str, bytes, reason_str
        );
    }
}

impl Drop for TraceSink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

pub(crate) struct LatencySink {
    writer: BufWriter<File>,
    wrote_header: bool,
}

impl LatencySink {
    pub(crate) fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            wrote_header: false,
        })
    }

    pub(crate) fn write_gmem(
        &mut self,
        cycle: Cycle,
        core: usize,
        warp: usize,
        request_id: u64,
        bytes: u32,
        issue_at: Cycle,
        latency: Cycle,
        l0_hit: bool,
        l1_hit: bool,
        l2_hit: bool,
    ) {
        if !self.wrote_header {
            let _ = writeln!(
                self.writer,
                "cycle,core,warp,mem_type,request_id,bytes,issue_at,latency,l0_hit,l1_hit,l2_hit"
            );
            self.wrote_header = true;
        }
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{},{},{},{},{},{},{}",
            cycle,
            core,
            warp,
            "gmem",
            request_id,
            bytes,
            issue_at,
            latency,
            l0_hit as u8,
            l1_hit as u8,
            l2_hit as u8
        );
    }

    pub(crate) fn write_smem(
        &mut self,
        cycle: Cycle,
        core: usize,
        warp: usize,
        request_id: u64,
        bytes: u32,
        issue_at: Cycle,
        latency: Cycle,
    ) {
        if !self.wrote_header {
            let _ = writeln!(
                self.writer,
                "cycle,core,warp,mem_type,request_id,bytes,issue_at,latency,l0_hit,l1_hit,l2_hit"
            );
            self.wrote_header = true;
        }
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{},{},{},{},,,",
            cycle,
            core,
            warp,
            "smem",
            request_id,
            bytes,
            issue_at,
            latency
        );
    }
}

impl Drop for LatencySink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

pub(crate) struct SmemConflictSink {
    writer: BufWriter<File>,
    wrote_header: bool,
}

impl SmemConflictSink {
    pub(crate) fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            wrote_header: false,
        })
    }

    pub(crate) fn write_row(
        &mut self,
        cycle: Cycle,
        core: usize,
        warp: usize,
        request_id: u64,
        active_lanes: u32,
        unique_banks: u32,
        unique_subbanks: u32,
        conflict_lanes: u32,
    ) {
        if !self.wrote_header {
            let _ = writeln!(
                self.writer,
                "cycle,core,warp,request_id,active_lanes,unique_banks,unique_subbanks,conflict_lanes,conflict_rate"
            );
            self.wrote_header = true;
        }
        let rate = if active_lanes == 0 {
            0.0
        } else {
            conflict_lanes as f64 / active_lanes as f64
        };
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{},{},{},{},{:.6}",
            cycle,
            core,
            warp,
            request_id,
            active_lanes,
            unique_banks,
            unique_subbanks,
            conflict_lanes,
            rate
        );
    }
}

impl Drop for SmemConflictSink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

pub(crate) struct SchedulerSink {
    writer: BufWriter<File>,
    wrote_header: bool,
}

impl SchedulerSink {
    pub(crate) fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            wrote_header: false,
        })
    }

    pub(crate) fn write_row(
        &mut self,
        cycle: Cycle,
        active_warps: u32,
        eligible_warps: u32,
        issued_warps: u32,
        issue_width: u32,
    ) {
        if !self.wrote_header {
            let _ = writeln!(
                self.writer,
                "cycle,active_warps,eligible_warps,issued_warps,issue_width"
            );
            self.wrote_header = true;
        }
        let _ = writeln!(
            self.writer,
            "{},{},{},{},{}",
            cycle, active_warps, eligible_warps, issued_warps, issue_width
        );
    }
}

impl Drop for SchedulerSink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}
