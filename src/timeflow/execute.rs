use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExecutePipelineConfig {
    pub alu: ServerConfig,
    pub int_mul: ServerConfig,
    pub int_div: ServerConfig,
    pub fp: ServerConfig,
    pub sfu: ServerConfig,
}

impl Default for ExecutePipelineConfig {
    fn default() -> Self {
        Self {
            alu: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 16,
                queue_capacity: 4,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
            int_mul: ServerConfig {
                base_latency: 3,
                bytes_per_cycle: 16,
                queue_capacity: 2,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
            int_div: ServerConfig {
                base_latency: 16,
                bytes_per_cycle: 8,
                queue_capacity: 2,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
            fp: ServerConfig {
                base_latency: 4,
                bytes_per_cycle: 8,
                queue_capacity: 4,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
            sfu: ServerConfig {
                base_latency: 8,
                bytes_per_cycle: 16,
                queue_capacity: 2,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
        }
    }
}


#[derive(Debug, Clone, Copy)]
pub enum ExecUnitKind {
    Int,
    IntMul,
    IntDiv,
    Fp,
    Sfu,
}

pub struct ExecutePipeline {
    alu: TimedServer<()>,
    int_mul: TimedServer<()>,
    int_div: TimedServer<()>,
    fp: TimedServer<()>,
    sfu: TimedServer<()>,
}

impl ExecutePipeline {
    pub fn new(config: ExecutePipelineConfig) -> Self {
        Self {
            alu: TimedServer::new(config.alu),
            int_mul: TimedServer::new(config.int_mul),
            int_div: TimedServer::new(config.int_div),
            fp: TimedServer::new(config.fp),
            sfu: TimedServer::new(config.sfu),
        }
    }

    pub fn tick(&mut self, now: Cycle) {
        self.alu.service_ready(now, |_| {});
        self.int_mul.service_ready(now, |_| {});
        self.int_div.service_ready(now, |_| {});
        self.fp.service_ready(now, |_| {});
        self.sfu.service_ready(now, |_| {});
    }

    pub fn issue(
        &mut self,
        now: Cycle,
        kind: ExecUnitKind,
        active_lanes: u32,
    ) -> Result<Ticket, Backpressure<()>> {
        let size_bytes = active_lanes.max(1);
        let request = ServiceRequest::new((), size_bytes);
        match kind {
            ExecUnitKind::Int => self.alu.try_enqueue(now, request),
            ExecUnitKind::IntMul => self.int_mul.try_enqueue(now, request),
            ExecUnitKind::IntDiv => self.int_div.try_enqueue(now, request),
            ExecUnitKind::Fp => self.fp.try_enqueue(now, request),
            ExecUnitKind::Sfu => self.sfu.try_enqueue(now, request),
        }
    }

    pub fn is_busy(&self, kind: ExecUnitKind) -> bool {
        match kind {
            ExecUnitKind::Int => self.alu.outstanding() > 0,
            ExecUnitKind::IntMul => self.int_mul.outstanding() > 0,
            ExecUnitKind::IntDiv => self.int_div.outstanding() > 0,
            ExecUnitKind::Fp => self.fp.outstanding() > 0,
            ExecUnitKind::Sfu => self.sfu.outstanding() > 0,
        }
    }

    pub fn suggest_retry(&self, kind: ExecUnitKind) -> Cycle {
        fn queue_full_retry(server: &TimedServer<()>) -> Cycle {
            server
                .oldest_ticket()
                .map(|ticket| ticket.ready_at())
                .unwrap_or_else(|| server.available_at())
        }
        match kind {
            ExecUnitKind::Int => queue_full_retry(&self.alu),
            ExecUnitKind::IntMul => queue_full_retry(&self.int_mul),
            ExecUnitKind::IntDiv => queue_full_retry(&self.int_div),
            ExecUnitKind::Fp => queue_full_retry(&self.fp),
            ExecUnitKind::Sfu => queue_full_retry(&self.sfu),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeq::Backpressure;

    #[test]
    fn execute_pipeline_busy_blocks_queue() {
        let config = ExecutePipelineConfig {
            alu: ServerConfig {
                base_latency: 1,
                bytes_per_cycle: 1,
                queue_capacity: 1,
                completions_per_cycle: 1,
                warmup_latency: 0,
            },
            ..Default::default()
        };
        let mut pipeline = ExecutePipeline::new(config);
        let now = 0;
        let ticket = pipeline
            .issue(now, ExecUnitKind::Int, 8)
            .expect("first request accepts");
        assert!(ticket.ready_at() > now);

        let err = pipeline
            .issue(now, ExecUnitKind::Int, 8)
            .expect_err("second request should queue-full");
        match err {
            Backpressure::QueueFull { .. } => {}
            _ => panic!("unexpected backpressure reason"),
        }
    }
}
