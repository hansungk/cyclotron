use crate::timeflow::gmem::{
    GmemCompletion, GmemFlowConfig, GmemIssue, GmemReject, GmemRequest, GmemStats, GmemSubgraph,
};
use crate::timeflow::graph::FlowGraph;
use crate::timeflow::barrier::BarrierConfig;
use crate::timeflow::dma::DmaConfig;
use crate::timeflow::fence::FenceConfig;
use crate::timeflow::icache::IcacheFlowConfig;
use crate::timeflow::lsu::LsuFlowConfig;
use crate::timeflow::operand_fetch::OperandFetchConfig;
use crate::timeflow::smem::{
    SmemCompletion, SmemFlowConfig, SmemIssue, SmemReject, SmemRequest, SmemStats, SmemSubgraph,
    SmemUtilSample,
};
use crate::timeflow::tensor::TensorConfig;
use crate::timeflow::warp_scheduler::WarpSchedulerConfig;
use crate::timeflow::writeback::WritebackConfig;
use crate::timeflow::types::CoreFlowPayload;
use crate::timeq::Cycle;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CoreGraphConfig {
    #[serde(default)]
    pub gmem: GmemFlowConfig,
    #[serde(default)]
    pub smem: SmemFlowConfig,
    #[serde(default)]
    pub lsu: LsuFlowConfig,
    #[serde(default)]
    pub icache: IcacheFlowConfig,
    #[serde(default)]
    pub writeback: WritebackConfig,
    #[serde(default)]
    pub operand_fetch: OperandFetchConfig,
    #[serde(default)]
    pub barrier: BarrierConfig,
    #[serde(default)]
    pub fence: FenceConfig,
    #[serde(default)]
    pub dma: DmaConfig,
    #[serde(default)]
    pub tensor: TensorConfig,
    #[serde(default)]
    pub scheduler: WarpSchedulerConfig,
}

impl Default for CoreGraphConfig {
    fn default() -> Self {
        Self {
            gmem: GmemFlowConfig::default(),
            smem: SmemFlowConfig::default(),
            lsu: LsuFlowConfig::default(),
            icache: IcacheFlowConfig::default(),
            writeback: WritebackConfig::default(),
            operand_fetch: OperandFetchConfig::default(),
            barrier: BarrierConfig::default(),
            fence: FenceConfig::default(),
            dma: DmaConfig::default(),
            tensor: TensorConfig::default(),
            scheduler: WarpSchedulerConfig::default(),
        }
    }
}

pub struct CoreGraph {
    graph: FlowGraph<CoreFlowPayload>,
    gmem: GmemSubgraph,
    smem: SmemSubgraph,
}

impl CoreGraph {
    pub fn new(config: CoreGraphConfig) -> Self {
        let mut graph = FlowGraph::new();
        let gmem = GmemSubgraph::attach(&mut graph, &config.gmem);
        let smem = SmemSubgraph::attach(&mut graph, &config.smem);
        Self { graph, gmem, smem }
    }

    pub fn issue_gmem(
        &mut self,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        self.gmem.issue(&mut self.graph, now, request)
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
        self.gmem.collect_completions(&mut self.graph, now);
        self.smem.collect_completions(&mut self.graph, now);
    }

    pub fn pop_gmem_completion(&mut self) -> Option<GmemCompletion> {
        self.gmem.completions.pop_front()
    }

    pub fn pending_gmem_completions(&self) -> usize {
        self.gmem.completions.len()
    }

    pub fn gmem_stats(&self) -> GmemStats {
        self.gmem.stats
    }

    pub fn clear_gmem_stats(&mut self) {
        self.gmem.stats = GmemStats::default();
    }

    pub fn pop_smem_completion(&mut self) -> Option<SmemCompletion> {
        self.smem.completions.pop_front()
    }

    pub fn pending_smem_completions(&self) -> usize {
        self.smem.completions.len()
    }

    pub fn smem_stats(&self) -> SmemStats {
        self.smem.stats
    }

    pub fn clear_smem_stats(&mut self) {
        self.smem.stats = SmemStats::default();
    }

    pub fn sample_smem_utilization(&mut self) -> SmemUtilSample {
        self.smem.sample_utilization(&mut self.graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_graph_completes_gmem_requests() {
        let mut cfg = CoreGraphConfig::default();
        cfg.gmem.nodes.coalescer.base_latency = 1;
        cfg.gmem.nodes.l0d_tag.base_latency = 2;
        cfg.gmem.nodes.dram.base_latency = 3;

        let mut graph = CoreGraph::new(cfg);
        let request = GmemRequest::new(0, 16, 0xF, true);
        let issue = graph.issue_gmem(0, request).expect("issue should succeed");
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(500) {
            graph.tick(cycle);
        }
        assert_eq!(1, graph.pending_gmem_completions());
    }
}
