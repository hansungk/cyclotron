use crate::timeflow::barrier::BarrierConfig;
use crate::timeflow::barrier::BarrierManager;
use crate::timeflow::dma::DmaConfig;
use crate::timeflow::dma::{DmaQueue, DmaReject};
use crate::timeflow::fence::FenceConfig;
use crate::timeflow::fence::{FenceIssue, FenceQueue, FenceReject, FenceRequest};
use crate::timeflow::gmem::GmemFlowConfig;
use crate::timeflow::gmem::{
    ClusterGmemGraph, GmemCompletion, GmemIssue, GmemReject, GmemRequest, GmemStats,
};
use crate::timeflow::graph::FlowGraph;
use crate::timeflow::icache::IcacheFlowConfig;
use crate::timeflow::icache::{
    IcacheIssue, IcacheReject, IcacheRequest, IcacheStats, IcacheSubgraph,
};
use crate::timeflow::lsu::LsuFlowConfig;
use crate::timeflow::lsu::LsuPayload;
use crate::timeflow::lsu::{LsuCompletion, LsuIssue, LsuReject, LsuStats, LsuSubgraph};
use crate::timeflow::operand_fetch::OperandFetchConfig;
use crate::timeflow::operand_fetch::{OperandFetchQueue, OperandFetchReject};
use crate::timeflow::smem::{
    SmemCompletion, SmemFlowConfig, SmemIssue, SmemReject, SmemRequest, SmemStats, SmemSubgraph,
    SmemUtilSample,
};
use crate::timeflow::tensor::{TensorConfig, TensorQueue, TensorReject};
use crate::timeflow::types::CoreFlowPayload;
use crate::timeflow::warp_scheduler::WarpSchedulerConfig;
use crate::timeflow::writeback::WritebackConfig;
use crate::timeflow::writeback::{
    WritebackIssue, WritebackPayload, WritebackQueue, WritebackReject,
};
use crate::timeflow::{
    execute::ExecUnitKind, execute::ExecutePipeline, execute::ExecutePipelineConfig,
};
use crate::timeq::{Backpressure, Cycle, Ticket};
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
    subgraphs: Vec<CoreSubgraph>,
    smem_index: usize,
    icache_index: usize,
    lsu_index: usize,
    operand_fetch_index: usize,
    dma_index: usize,
    tensor_index: usize,
    execute_index: usize,
    writeback_index: usize,
    fence_index: usize,
    barrier_index: usize,
    cluster_gmem_index: Option<usize>,
}

impl CoreGraph {
    pub fn new(
        config: CoreGraphConfig,
        num_warps: usize,
        cluster_gmem: Option<std::sync::Arc<std::sync::RwLock<ClusterGmemGraph>>>,
    ) -> Self {
        let mut graph = FlowGraph::new();
        let smem = SmemSubgraph::attach(&mut graph, &config.memory.smem);
        let icache = IcacheSubgraph::new(config.memory.icache);
        let lsu = LsuSubgraph::new(config.memory.lsu, num_warps);
        let operand_fetch = OperandFetchQueue::new(
            config.memory.operand_fetch.enabled,
            config.memory.operand_fetch.queue,
        );
        let dma = DmaQueue::new(config.io.dma);
        let tensor = TensorQueue::new(config.compute.tensor);
        let execute = ExecutePipeline::new(config.compute.execute);
        let writeback = WritebackQueue::new(config.memory.writeback);
        let fence = FenceQueue::new(config.io.fence);
        let barrier = BarrierManager::new(config.io.barrier, num_warps);

        // Subgraph ordering matters for tick-front; keep it aligned with the historical tick order.
        let mut subgraphs = Vec::with_capacity(11);
        let smem_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Smem(smem));
        let icache_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Icache(icache));
        let lsu_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Lsu(lsu));
        let operand_fetch_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::OperandFetch(operand_fetch));
        let dma_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Dma(dma));
        let tensor_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Tensor(tensor));
        let execute_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Execute(execute));
        let writeback_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Writeback(writeback));
        let fence_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Fence(fence));
        let barrier_index = subgraphs.len();
        subgraphs.push(CoreSubgraph::Barrier(barrier));
        let cluster_gmem_index = cluster_gmem.map(|handle| {
            let idx = subgraphs.len();
            subgraphs.push(CoreSubgraph::ClusterGmem(handle));
            idx
        });

        Self {
            graph,
            subgraphs,
            smem_index,
            icache_index,
            lsu_index,
            operand_fetch_index,
            dma_index,
            tensor_index,
            execute_index,
            writeback_index,
            fence_index,
            barrier_index,
            cluster_gmem_index,
        }
    }

    pub fn issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        self.with_smem_mut(|smem, graph| smem.issue(graph, now, request))
    }

    /// Tick standalone timing units that are not attached to the central `FlowGraph` (yet).
    ///
    /// Ordering matters: we keep this aligned with the historical tick order inside
    /// `CoreTimingModel::tick` to avoid subtle same-cycle behavior changes.
    pub fn tick_front(&mut self, now: Cycle) {
        for subgraph in &mut self.subgraphs {
            subgraph.tick_phase(TickPhase::Front, now);
        }
    }

    /// Tick the central `FlowGraph` and allow graph-attached subgraphs to collect completions.
    ///
    /// This is intentionally separate from `tick_front()` so callers can preserve same-cycle
    /// completion behavior for requests that are issued into the graph on `now`.
    pub fn tick_graph(&mut self, now: Cycle) {
        self.graph.tick(now);
        for subgraph in &mut self.subgraphs {
            subgraph.collect_completions(&mut self.graph, now);
        }
    }

    /// Tick standalone units that must run after enqueuing completion-driven work (writeback/fence).
    pub fn tick_back(&mut self, now: Cycle) {
        for subgraph in &mut self.subgraphs {
            subgraph.tick_phase(TickPhase::Back, now);
        }
    }

    pub fn collect_cluster_gmem_completions(&self, core_id: usize) -> Vec<GmemCompletion> {
        let Some(cluster) = self.cluster_gmem_ref() else {
            return Vec::new();
        };
        let mut completions = Vec::new();
        let mut handle = cluster.write().unwrap();
        while let Some(completion) = handle.pop_completion(core_id) {
            completions.push(completion);
        }
        completions
    }

    pub fn pop_smem_completion(&mut self) -> Option<SmemCompletion> {
        self.smem_mut().completions.pop_front()
    }

    pub fn pending_smem_completions(&self) -> usize {
        self.smem_ref().completions.len()
    }

    pub fn smem_stats(&self) -> SmemStats {
        match self.subgraphs[self.smem_index].stats_snapshot() {
            CoreSubgraphStats::Smem(stats) => stats,
            _ => unreachable!("smem index always points to smem"),
        }
    }

    pub fn clear_smem_stats(&mut self) {
        self.subgraphs[self.smem_index].clear_stats();
    }

    pub fn operand_fetch_try_issue(
        &mut self,
        now: Cycle,
        bytes: u32,
    ) -> Result<Ticket, OperandFetchReject> {
        self.with_operand_fetch_mut(|q| q.try_issue(now, (), bytes))
    }

    pub fn issue_icache(
        &mut self,
        now: Cycle,
        request: IcacheRequest,
    ) -> Result<IcacheIssue, IcacheReject> {
        self.with_icache_mut(|icache| icache.issue(now, request))
    }

    pub fn icache_stats(&self) -> IcacheStats {
        match self.subgraphs[self.icache_index].stats_snapshot() {
            CoreSubgraphStats::Icache(stats) => stats,
            _ => unreachable!("icache index always points to icache"),
        }
    }

    pub fn clear_icache_stats(&mut self) {
        self.subgraphs[self.icache_index].clear_stats();
    }

    pub fn writeback_try_issue(
        &mut self,
        now: Cycle,
        payload: WritebackPayload,
    ) -> Result<WritebackIssue, WritebackReject> {
        self.with_writeback_mut(|wb| wb.try_issue(now, payload))
    }

    pub fn writeback_pop_ready(&mut self) -> Option<WritebackPayload> {
        self.with_writeback_mut(|wb| wb.pop_ready())
    }

    pub fn fence_try_issue(
        &mut self,
        now: Cycle,
        request: FenceRequest,
    ) -> Result<FenceIssue, FenceReject> {
        self.with_fence_mut(|f| f.try_issue(now, request))
    }

    pub fn fence_pop_ready(&mut self) -> Option<FenceRequest> {
        self.with_fence_mut(|f| f.pop_ready())
    }

    pub fn fence_is_enabled(&self) -> bool {
        self.fence_ref().is_enabled()
    }

    pub fn dma_try_issue(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, DmaReject> {
        self.with_dma_mut(|dma| dma.try_issue(now, bytes))
    }

    pub fn dma_is_busy(&self) -> bool {
        self.dma_ref().is_busy()
    }

    pub fn dma_bytes_issued(&self) -> u64 {
        self.dma_ref().bytes_issued()
    }

    pub fn dma_bytes_completed(&self) -> u64 {
        self.dma_ref().bytes_completed()
    }

    pub fn dma_completed(&self) -> u64 {
        self.dma_ref().completed()
    }

    pub fn dma_matches_mmio(&self, addr: u64) -> bool {
        self.dma_ref().matches_mmio(addr)
    }

    pub fn dma_matches_csr(&self, addr: u32) -> bool {
        self.dma_ref().matches_csr(addr)
    }

    pub fn tensor_try_issue(&mut self, now: Cycle, bytes: u32) -> Result<Ticket, TensorReject> {
        self.with_tensor_mut(|tensor| tensor.try_issue(now, bytes))
    }

    pub fn tensor_is_busy(&self) -> bool {
        self.tensor_ref().is_busy()
    }

    pub fn tensor_bytes_issued(&self) -> u64 {
        self.tensor_ref().bytes_issued()
    }

    pub fn tensor_bytes_completed(&self) -> u64 {
        self.tensor_ref().bytes_completed()
    }

    pub fn tensor_completed(&self) -> u64 {
        self.tensor_ref().completed()
    }

    pub fn tensor_matches_mmio(&self, addr: u64) -> bool {
        self.tensor_ref().matches_mmio(addr)
    }

    pub fn tensor_matches_csr(&self, addr: u32) -> bool {
        self.tensor_ref().matches_csr(addr)
    }

    pub fn execute_issue(
        &mut self,
        now: Cycle,
        kind: ExecUnitKind,
        active_lanes: u32,
    ) -> Result<Ticket, Backpressure<()>> {
        self.with_execute_mut(|exec| exec.issue(now, kind, active_lanes))
    }

    pub fn execute_is_busy(&self, kind: ExecUnitKind) -> bool {
        self.execute_ref().is_busy(kind)
    }

    pub fn execute_suggest_retry(&self, kind: ExecUnitKind) -> Cycle {
        self.execute_ref().suggest_retry(kind)
    }

    pub fn barrier_is_enabled(&self) -> bool {
        self.barrier_ref().is_enabled()
    }

    pub fn barrier_arrive(&mut self, now: Cycle, warp: usize, barrier_id: u32) -> Option<Cycle> {
        self.with_barrier_mut(|b| b.arrive(now, warp, barrier_id))
    }

    pub fn barrier_tick(&mut self, now: Cycle) -> Option<Vec<usize>> {
        self.with_barrier_mut(|b| b.tick(now))
    }

    pub fn cluster_gmem_issue(
        &self,
        core_id: usize,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        let Some(cluster) = self.cluster_gmem_ref() else {
            return Err(GmemReject::new(
                request,
                now.saturating_add(1),
                crate::timeflow::GmemRejectReason::QueueFull,
            ));
        };
        cluster.write().unwrap().issue(core_id, now, request)
    }

    pub fn cluster_gmem_stats(&self, core_id: usize) -> GmemStats {
        self.cluster_gmem_ref()
            .map(|cluster| cluster.read().unwrap().stats(core_id))
            .unwrap_or_default()
    }

    pub fn cluster_gmem_clear_stats(&self, core_id: usize) {
        if let Some(cluster) = self.cluster_gmem_ref() {
            cluster.write().unwrap().clear_stats(core_id);
        }
    }

    pub fn cluster_gmem_hierarchy_stats_per_level(&self) -> (GmemStats, GmemStats, GmemStats) {
        self.cluster_gmem_ref()
            .map(|cluster| cluster.read().unwrap().hierarchy_stats_per_level())
            .unwrap_or_default()
    }

    pub fn lsu_issue_gmem(
        &mut self,
        now: Cycle,
        request: crate::timeflow::GmemRequest,
    ) -> Result<LsuIssue, LsuReject> {
        self.with_lsu_mut(|lsu| lsu.issue_gmem(now, request))
    }

    pub fn lsu_issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<LsuIssue, LsuReject> {
        self.with_lsu_mut(|lsu| lsu.issue_smem(now, request))
    }

    pub fn lsu_peek_ready(&mut self, now: Cycle) -> Option<LsuPayload> {
        self.with_lsu_mut(|lsu| lsu.peek_ready(now))
    }

    pub fn lsu_take_ready(&mut self, now: Cycle) -> Option<LsuCompletion<LsuPayload>> {
        self.with_lsu_mut(|lsu| lsu.take_ready(now))
    }

    pub fn lsu_release_issue_resources(&mut self, payload: &LsuPayload) {
        self.with_lsu_mut(|lsu| lsu.release_issue_resources(payload));
    }

    pub fn lsu_can_reserve_load_data(&self, payload: &LsuPayload) -> bool {
        self.lsu_ref().can_reserve_load_data(payload)
    }

    pub fn lsu_reserve_load_data(&mut self, payload: &LsuPayload) -> bool {
        self.with_lsu_mut(|lsu| lsu.reserve_load_data(payload))
    }

    pub fn lsu_release_load_data(&mut self, payload: &LsuPayload) {
        self.with_lsu_mut(|lsu| lsu.release_load_data(payload));
    }

    pub fn lsu_stats(&self) -> LsuStats {
        match self.subgraphs[self.lsu_index].stats_snapshot() {
            CoreSubgraphStats::Lsu(stats) => stats,
            _ => unreachable!("lsu index always points to lsu"),
        }
    }

    pub fn clear_lsu_stats(&mut self) {
        self.subgraphs[self.lsu_index].clear_stats();
    }

    pub fn sample_smem_utilization(&mut self) -> SmemUtilSample {
        self.with_smem_mut(|smem, graph| smem.sample_utilization(graph))
    }

    /// Record one SMEM sample cycle into SMEM statistics (busy samples per bank).
    pub fn record_smem_sample(&mut self) {
        self.with_smem_mut(|smem, graph| smem.sample_and_accumulate(graph));
    }

    fn smem_ref(&self) -> &SmemSubgraph {
        match &self.subgraphs[self.smem_index] {
            CoreSubgraph::Smem(smem) => smem,
            _ => unreachable!("smem index always points to smem"),
        }
    }

    fn smem_mut(&mut self) -> &mut SmemSubgraph {
        match &mut self.subgraphs[self.smem_index] {
            CoreSubgraph::Smem(smem) => smem,
            _ => unreachable!("smem index always points to smem"),
        }
    }

    fn with_smem_mut<R>(
        &mut self,
        f: impl FnOnce(&mut SmemSubgraph, &mut FlowGraph<CoreFlowPayload>) -> R,
    ) -> R {
        let smem_index = self.smem_index;
        let graph = &mut self.graph;
        match &mut self.subgraphs[smem_index] {
            CoreSubgraph::Smem(smem) => f(smem, graph),
            _ => unreachable!("smem index always points to smem"),
        }
    }

    fn lsu_ref(&self) -> &LsuSubgraph {
        match &self.subgraphs[self.lsu_index] {
            CoreSubgraph::Lsu(lsu) => lsu,
            _ => unreachable!("lsu index always points to lsu"),
        }
    }

    fn with_lsu_mut<R>(&mut self, f: impl FnOnce(&mut LsuSubgraph) -> R) -> R {
        match &mut self.subgraphs[self.lsu_index] {
            CoreSubgraph::Lsu(lsu) => f(lsu),
            _ => unreachable!("lsu index always points to lsu"),
        }
    }

    fn with_icache_mut<R>(&mut self, f: impl FnOnce(&mut IcacheSubgraph) -> R) -> R {
        match &mut self.subgraphs[self.icache_index] {
            CoreSubgraph::Icache(icache) => f(icache),
            _ => unreachable!("icache index always points to icache"),
        }
    }

    fn with_operand_fetch_mut<R>(&mut self, f: impl FnOnce(&mut OperandFetchQueue) -> R) -> R {
        match &mut self.subgraphs[self.operand_fetch_index] {
            CoreSubgraph::OperandFetch(q) => f(q),
            _ => unreachable!("operand_fetch index always points to operand_fetch"),
        }
    }

    fn dma_ref(&self) -> &DmaQueue {
        match &self.subgraphs[self.dma_index] {
            CoreSubgraph::Dma(dma) => dma,
            _ => unreachable!("dma index always points to dma"),
        }
    }

    fn with_dma_mut<R>(&mut self, f: impl FnOnce(&mut DmaQueue) -> R) -> R {
        match &mut self.subgraphs[self.dma_index] {
            CoreSubgraph::Dma(dma) => f(dma),
            _ => unreachable!("dma index always points to dma"),
        }
    }

    fn tensor_ref(&self) -> &TensorQueue {
        match &self.subgraphs[self.tensor_index] {
            CoreSubgraph::Tensor(tensor) => tensor,
            _ => unreachable!("tensor index always points to tensor"),
        }
    }

    fn with_tensor_mut<R>(&mut self, f: impl FnOnce(&mut TensorQueue) -> R) -> R {
        match &mut self.subgraphs[self.tensor_index] {
            CoreSubgraph::Tensor(tensor) => f(tensor),
            _ => unreachable!("tensor index always points to tensor"),
        }
    }

    fn execute_ref(&self) -> &ExecutePipeline {
        match &self.subgraphs[self.execute_index] {
            CoreSubgraph::Execute(exec) => exec,
            _ => unreachable!("execute index always points to execute"),
        }
    }

    fn with_execute_mut<R>(&mut self, f: impl FnOnce(&mut ExecutePipeline) -> R) -> R {
        match &mut self.subgraphs[self.execute_index] {
            CoreSubgraph::Execute(exec) => f(exec),
            _ => unreachable!("execute index always points to execute"),
        }
    }

    fn with_writeback_mut<R>(&mut self, f: impl FnOnce(&mut WritebackQueue) -> R) -> R {
        match &mut self.subgraphs[self.writeback_index] {
            CoreSubgraph::Writeback(wb) => f(wb),
            _ => unreachable!("writeback index always points to writeback"),
        }
    }

    fn with_fence_mut<R>(&mut self, f: impl FnOnce(&mut FenceQueue) -> R) -> R {
        match &mut self.subgraphs[self.fence_index] {
            CoreSubgraph::Fence(fence) => f(fence),
            _ => unreachable!("fence index always points to fence"),
        }
    }

    fn fence_ref(&self) -> &FenceQueue {
        match &self.subgraphs[self.fence_index] {
            CoreSubgraph::Fence(fence) => fence,
            _ => unreachable!("fence index always points to fence"),
        }
    }

    fn barrier_ref(&self) -> &BarrierManager {
        match &self.subgraphs[self.barrier_index] {
            CoreSubgraph::Barrier(barrier) => barrier,
            _ => unreachable!("barrier index always points to barrier"),
        }
    }

    fn with_barrier_mut<R>(&mut self, f: impl FnOnce(&mut BarrierManager) -> R) -> R {
        match &mut self.subgraphs[self.barrier_index] {
            CoreSubgraph::Barrier(barrier) => f(barrier),
            _ => unreachable!("barrier index always points to barrier"),
        }
    }

    fn cluster_gmem_ref(&self) -> Option<&std::sync::Arc<std::sync::RwLock<ClusterGmemGraph>>> {
        let idx = self.cluster_gmem_index?;
        match &self.subgraphs[idx] {
            CoreSubgraph::ClusterGmem(handle) => Some(handle),
            _ => None,
        }
    }
}

enum CoreSubgraph {
    Smem(SmemSubgraph),
    Icache(IcacheSubgraph),
    Lsu(LsuSubgraph),
    OperandFetch(OperandFetchQueue),
    Dma(DmaQueue),
    Tensor(TensorQueue),
    Execute(ExecutePipeline),
    Writeback(WritebackQueue),
    Fence(FenceQueue),
    Barrier(BarrierManager),
    ClusterGmem(std::sync::Arc<std::sync::RwLock<ClusterGmemGraph>>),
}

enum CoreSubgraphStats {
    Smem(SmemStats),
    Icache(IcacheStats),
    Lsu(LsuStats),
}

#[derive(Debug, Clone, Copy)]
enum TickPhase {
    Front,
    Back,
}

impl CoreSubgraph {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        match self {
            CoreSubgraph::Smem(_) => {}
            CoreSubgraph::Icache(icache) => {
                if matches!(phase, TickPhase::Front) {
                    icache.tick(now);
                }
            }
            CoreSubgraph::Lsu(lsu) => {
                if matches!(phase, TickPhase::Front) {
                    lsu.tick(now);
                }
            }
            CoreSubgraph::OperandFetch(q) => {
                if matches!(phase, TickPhase::Front) {
                    q.tick(now, |_| {});
                }
            }
            CoreSubgraph::Dma(dma) => {
                if matches!(phase, TickPhase::Front) {
                    dma.tick(now);
                }
            }
            CoreSubgraph::Tensor(tensor) => {
                if matches!(phase, TickPhase::Front) {
                    tensor.tick(now);
                }
            }
            CoreSubgraph::Execute(exec) => {
                if matches!(phase, TickPhase::Front) {
                    exec.tick(now);
                }
            }
            CoreSubgraph::Writeback(wb) => {
                if matches!(phase, TickPhase::Back) {
                    wb.tick(now);
                }
            }
            CoreSubgraph::Fence(fence) => {
                if matches!(phase, TickPhase::Back) {
                    fence.tick(now);
                }
            }
            CoreSubgraph::Barrier(_) => {}
            CoreSubgraph::ClusterGmem(handle) => {
                if matches!(phase, TickPhase::Front) {
                    handle.write().unwrap().tick(now);
                }
            }
        }
    }

    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        match self {
            CoreSubgraph::Smem(smem) => smem.collect_completions(graph, now),
            CoreSubgraph::Icache(_) => {}
            CoreSubgraph::Lsu(_) => {}
            CoreSubgraph::OperandFetch(_) => {}
            CoreSubgraph::Dma(_) => {}
            CoreSubgraph::Tensor(_) => {}
            CoreSubgraph::Execute(_) => {}
            CoreSubgraph::Writeback(_) => {}
            CoreSubgraph::Fence(_) => {}
            CoreSubgraph::Barrier(_) => {}
            CoreSubgraph::ClusterGmem(_) => {}
        }
    }

    fn stats_snapshot(&self) -> CoreSubgraphStats {
        match self {
            CoreSubgraph::Smem(smem) => CoreSubgraphStats::Smem(smem.stats.clone()),
            CoreSubgraph::Icache(icache) => CoreSubgraphStats::Icache(icache.stats()),
            CoreSubgraph::Lsu(lsu) => CoreSubgraphStats::Lsu(lsu.stats()),
            _ => unreachable!("stats_snapshot called for a subgraph without stats"),
        }
    }

    fn clear_stats(&mut self) {
        match self {
            CoreSubgraph::Smem(smem) => smem.stats = SmemStats::default(),
            CoreSubgraph::Lsu(lsu) => lsu.clear_stats(),
            CoreSubgraph::Icache(icache) => icache.clear_stats(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeflow::gmem::GmemRequest;

    #[test]
    fn core_graph_completes_smem_requests() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.smem.lane.base_latency = 0;
        cfg.memory.smem.bank.base_latency = 0;
        cfg.memory.smem.crossbar.base_latency = 0;

        let mut graph = CoreGraph::new(cfg, 1, None);
        let request = SmemRequest::new(0, 16, 0xF, false, 0);
        let issue = graph.issue_smem(0, request).expect("issue should succeed");
        let ready_at = issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(100) {
            graph.tick_graph(cycle);
        }
        assert_eq!(1, graph.pending_smem_completions());
    }

    #[test]
    fn core_graph_tracks_smem_stats_correctly() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.smem.lane.base_latency = 0;
        cfg.memory.smem.bank.base_latency = 0;
        cfg.memory.smem.crossbar.base_latency = 0;

        let mut graph = CoreGraph::new(cfg, 1, None);
        graph.clear_smem_stats();
        let smem_req = SmemRequest::new(0, 16, 0xF, false, 0);
        let smem_issue = graph.issue_smem(0, smem_req).expect("smem issue");
        let ready_at = smem_issue.ticket.ready_at();
        for cycle in 0..=ready_at.saturating_add(200) {
            graph.tick_graph(cycle);
        }
        let smem_stats = graph.smem_stats();
        assert_eq!(smem_stats.issued, 1);
        assert_eq!(smem_stats.completed, 1);
    }

    #[test]
    fn core_graph_ticks_icache_lsu_and_collects_completions() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.icache.policy.hit_rate = 1.0;
        cfg.memory.icache.hit.base_latency = 0;
        cfg.memory.icache.hit.queue_capacity = 2;

        cfg.memory.lsu.issue.base_latency = 0;
        cfg.memory.lsu.issue.queue_capacity = 2;
        cfg.memory.lsu.queues.global_ldq.queue_capacity = 2;

        let mut graph = CoreGraph::new(cfg, 1, None);
        let icache_req = IcacheRequest::new(0, 0x100, 8);
        graph.issue_icache(0, icache_req).expect("icache issue");

        let mut gmem_req = GmemRequest::new(0, 16, 0xF, true);
        gmem_req.addr = 0x200;
        graph.lsu_issue_gmem(0, gmem_req).expect("lsu issue");

        let mut saw_ready = false;
        for cycle in 0..50 {
            graph.tick_front(cycle);
            if let Some(ready) = graph.lsu_peek_ready(cycle) {
                assert!(matches!(ready, LsuPayload::Gmem(_)));
                graph.lsu_take_ready(cycle);
                saw_ready = true;
                break;
            }
        }
        assert!(saw_ready, "expected LSU readiness within bounded cycles");

        let icache_stats = graph.icache_stats();
        assert_eq!(icache_stats.issued, 1);
    }

    #[test]
    fn core_graph_ticks_cluster_gmem_from_front_phase() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.gmem.policy.l0_enabled = false;

        cfg.memory.gmem.nodes.dram.queue_capacity = 4;
        cfg.memory.gmem.nodes.dram.base_latency = 1;

        let cluster = ClusterGmemGraph::new(cfg.memory.gmem.clone(), 1, 1);
        let cluster = std::sync::Arc::new(std::sync::RwLock::new(cluster));
        let mut graph = CoreGraph::new(cfg, 1, Some(cluster));

        let request = GmemRequest::new(0, 16, 0xF, true);
        graph.cluster_gmem_issue(0, 0, request).expect("gmem issue");

        let mut seen = false;
        for cycle in 0..200 {
            graph.tick_front(cycle);
            let completions = graph.collect_cluster_gmem_completions(0);
            if !completions.is_empty() {
                seen = true;
                break;
            }
        }
        assert!(seen, "expected a completion within bounded cycles");
    }

    #[test]
    fn core_graph_ticks_writeback_and_fence_in_back_phase() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.writeback.enabled = true;
        cfg.memory.writeback.queue.base_latency = 1;
        cfg.memory.writeback.queue.queue_capacity = 2;
        cfg.io.fence.enabled = true;
        cfg.io.fence.queue.base_latency = 1;
        cfg.io.fence.queue.queue_capacity = 2;

        let mut graph = CoreGraph::new(cfg, 1, None);
        let gmem_req = GmemRequest::new(0, 4, 0xF, true);
        let completion = GmemCompletion {
            request: gmem_req,
            ticket_ready_at: 0,
            completed_at: 0,
        };
        graph
            .writeback_try_issue(0, WritebackPayload::Gmem(completion))
            .expect("writeback issue");
        graph
            .fence_try_issue(
                0,
                FenceRequest {
                    warp: 0,
                    request_id: 7,
                },
            )
            .expect("fence issue");

        graph.tick_back(0);
        assert!(graph.writeback_pop_ready().is_none());
        assert!(graph.fence_pop_ready().is_none());

        graph.tick_back(1);
        assert!(graph.writeback_pop_ready().is_some());
        assert!(graph.fence_pop_ready().is_some());
    }

    #[test]
    fn core_graph_barrier_tick_releases_warps() {
        let mut cfg = CoreGraphConfig::default();
        cfg.io.barrier.enabled = true;
        cfg.io.barrier.expected_warps = Some(2);
        cfg.io.barrier.queue.base_latency = 1;

        let mut graph = CoreGraph::new(cfg, 2, None);
        assert!(graph.barrier_arrive(0, 0, 0).is_none());
        let release_at = graph.barrier_arrive(0, 1, 0).expect("barrier schedule");

        let released = graph.barrier_tick(release_at.saturating_sub(1));
        assert!(released.is_none());
        let released = graph.barrier_tick(release_at).expect("release");
        assert_eq!(released.len(), 2);
    }

    #[test]
    fn core_graph_ticks_dma_and_tensor_in_front_phase() {
        let mut cfg = CoreGraphConfig::default();
        cfg.io.dma.enabled = true;
        cfg.io.dma.queue.base_latency = 1;
        cfg.io.dma.queue.queue_capacity = 2;
        cfg.compute.tensor.enabled = true;
        cfg.compute.tensor.queue.base_latency = 1;
        cfg.compute.tensor.queue.queue_capacity = 2;

        let mut graph = CoreGraph::new(cfg, 1, None);
        graph.dma_try_issue(0, 64).expect("dma issue");
        graph.tensor_try_issue(0, 64).expect("tensor issue");

        let mut dma_done = false;
        let mut tensor_done = false;
        for cycle in 0..10 {
            graph.tick_front(cycle);
            if graph.dma_completed() > 0 {
                dma_done = true;
            }
            if graph.tensor_completed() > 0 {
                tensor_done = true;
            }
            if dma_done && tensor_done {
                break;
            }
        }
        assert!(dma_done, "expected DMA completion within bounded cycles");
        assert!(
            tensor_done,
            "expected tensor completion within bounded cycles"
        );
    }

    #[test]
    fn core_graph_ticks_execute_pipeline_in_front_phase() {
        let mut cfg = CoreGraphConfig::default();
        cfg.compute.execute.alu.base_latency = 1;
        cfg.compute.execute.alu.queue_capacity = 1;
        cfg.compute.execute.alu.bytes_per_cycle = 1;

        let mut graph = CoreGraph::new(cfg, 1, None);
        let ticket = graph
            .execute_issue(0, ExecUnitKind::Int, 8)
            .expect("execute issue");
        assert!(ticket.ready_at() > 0);

        assert!(graph.execute_is_busy(ExecUnitKind::Int));
        graph.tick_front(ticket.ready_at());
        assert!(!graph.execute_is_busy(ExecUnitKind::Int));
    }

    #[test]
    fn core_graph_collects_cluster_and_smem_completions_same_cycle() {
        let mut cfg = CoreGraphConfig::default();
        cfg.memory.smem.lane.base_latency = 0;
        cfg.memory.smem.bank.base_latency = 0;
        cfg.memory.smem.crossbar.base_latency = 0;
        cfg.memory.gmem.nodes.dram.queue_capacity = 4;
        cfg.memory.gmem.nodes.dram.base_latency = 1;
        cfg.memory.gmem.policy.l0_enabled = false;

        let cluster = ClusterGmemGraph::new(cfg.memory.gmem.clone(), 1, 1);
        let cluster = std::sync::Arc::new(std::sync::RwLock::new(cluster));
        let mut graph = CoreGraph::new(cfg, 1, Some(cluster));

        let smem_req = SmemRequest::new(0, 16, 0xF, false, 0);
        graph.issue_smem(0, smem_req).expect("smem issue");

        let gmem_req = GmemRequest::new(0, 16, 0xF, true);
        graph
            .cluster_gmem_issue(0, 0, gmem_req)
            .expect("gmem issue");

        let mut saw_smem = false;
        let mut saw_gmem = false;
        for cycle in 0..200 {
            graph.tick_front(cycle);
            graph.tick_graph(cycle);
            let smem_done = graph.pop_smem_completion().is_some();
            let gmem_done = !graph.collect_cluster_gmem_completions(0).is_empty();
            saw_smem |= smem_done;
            saw_gmem |= gmem_done;
            if saw_smem && saw_gmem {
                break;
            }
        }
        assert!(saw_smem, "expected smem completion");
        assert!(saw_gmem, "expected gmem completion");
    }
}
