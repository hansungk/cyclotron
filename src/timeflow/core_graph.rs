use crate::timeflow::barrier::BarrierConfig;
use crate::timeflow::dma::DmaConfig;
use crate::timeflow::fence::FenceConfig;
use crate::timeflow::gmem::GmemFlowConfig;
use crate::timeflow::graph::FlowGraph;
use crate::timeflow::icache::IcacheFlowConfig;
use crate::timeflow::lsu::LsuFlowConfig;
use crate::timeflow::operand_fetch::OperandFetchConfig;
use crate::timeflow::smem::{
    SmemCompletion, SmemFlowConfig, SmemIssue, SmemReject, SmemRequest, SmemStats, SmemSubgraph,
    SmemUtilSample,
};
use crate::timeflow::types::CoreFlowPayload;
use crate::timeflow::warp_scheduler::WarpSchedulerConfig;
use crate::timeflow::writeback::WritebackConfig;
use crate::timeflow::{execute::ExecutePipelineConfig, tensor::TensorConfig};
use crate::timeq::Cycle;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CoreGraphConfig {
    #[serde(flatten)]
    pub memory: MemoryConfig,
    #[serde(flatten)]
    pub compute: ComputeConfig,
    #[serde(flatten)]
    pub io: IoConfig,
}

impl Default for CoreGraphConfig {
    fn default() -> Self {
        Self {
            memory: MemoryConfig::default(),
            compute: ComputeConfig::default(),
            io: IoConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub gmem: GmemFlowConfig,
    pub smem: SmemFlowConfig,
    pub lsu: LsuFlowConfig,
    pub icache: IcacheFlowConfig,
    pub writeback: WritebackConfig,
    pub operand_fetch: OperandFetchConfig,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            gmem: GmemFlowConfig::default(),
            smem: SmemFlowConfig::default(),
            lsu: LsuFlowConfig::default(),
            icache: IcacheFlowConfig::default(),
            writeback: WritebackConfig::default(),
            operand_fetch: OperandFetchConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ComputeConfig {
    pub tensor: TensorConfig,
    pub scheduler: WarpSchedulerConfig,
    pub execute: ExecutePipelineConfig,
}

impl Default for ComputeConfig {
    fn default() -> Self {
        Self {
            tensor: TensorConfig::default(),
            scheduler: WarpSchedulerConfig::default(),
            execute: ExecutePipelineConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IoConfig {
    pub barrier: BarrierConfig,
    pub fence: FenceConfig,
    pub dma: DmaConfig,
}

impl Default for IoConfig {
    fn default() -> Self {
        Self {
            barrier: BarrierConfig::default(),
            fence: FenceConfig::default(),
            dma: DmaConfig::default(),
        }
    }
}

pub struct CoreGraph {
    pub(crate) graph: FlowGraph<CoreFlowPayload>,
    pub(crate) smem: SmemSubgraph,
}

impl CoreGraph {
    pub fn new(config: CoreGraphConfig) -> Self {
        let mut graph = FlowGraph::new();
        let smem = SmemSubgraph::attach(&mut graph, &config.memory.smem);
        Self { graph, smem }
    }

    pub fn issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        self.smem.issue(&mut self.graph, now, request)
    }

    pub fn tick(&mut self, now: Cycle) {
        self.graph.tick(now);
        self.smem.collect_completions(&mut self.graph, now);
    }

    pub fn pop_smem_completion(&mut self) -> Option<SmemCompletion> {
        self.smem.completions.pop_front()
    }

    pub fn pending_smem_completions(&self) -> usize {
        self.smem.completions.len()
    }

    pub fn smem_stats(&self) -> SmemStats {
        self.smem.stats.clone()
    }

    pub fn clear_smem_stats(&mut self) {
        self.smem.stats = SmemStats::default();
    }

    pub fn sample_smem_utilization(&mut self) -> SmemUtilSample {
        self.smem.sample_utilization(&mut self.graph)
    }

    /// Record one SMEM sample cycle into SMEM statistics (busy samples per bank).
    pub fn record_smem_sample(&mut self) {
        self.smem.sample_and_accumulate(&mut self.graph);
    }

    /// Minimal adapter: provide mutable access to the SMEM subgraph.
    pub(crate) fn smem_subgraph_mut(&mut self) -> &mut SmemSubgraph {
        &mut self.smem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_graph_completes_smem_requests() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.smem.lane.base_latency = 0;
        cfg.memory.smem.bank.base_latency = 0;
        cfg.memory.smem.crossbar.base_latency = 0;

        let mut graph = CoreGraph::new(cfg);
        let request = SmemRequest::new(0, 16, 0xF, false, 0);
        let issue = graph.issue_smem(0, request).expect("issue should succeed");
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(100) {
            graph.tick(cycle);
        }
        assert_eq!(1, graph.pending_smem_completions());
    }

    #[test]
    fn core_graph_tracks_smem_stats_correctly() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.smem.lane.base_latency = 0;
        cfg.memory.smem.bank.base_latency = 0;
        cfg.memory.smem.crossbar.base_latency = 0;

        let mut graph = CoreGraph::new(cfg);
        graph.clear_smem_stats();
        let smem_req = SmemRequest::new(0, 16, 0xF, false, 0);
        let smem_issue = graph.issue_smem(0, smem_req).expect("smem issue");
        let ready_at = smem_issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(200) {
            graph.tick(cycle);
        }
        let smem_stats = graph.smem_stats();
        assert_eq!(smem_stats.issued, 1);
        assert_eq!(smem_stats.completed, 1);
    }
}
