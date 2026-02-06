use crate::timeflow::{
    barrier::{BarrierConfig, BarrierManager, BarrierSummary},
    dma::{DmaConfig, DmaQueue, DmaReject},
    fence::{FenceConfig, FenceIssue, FenceQueue, FenceReject, FenceRequest},
    gmem::{ClusterGmemGraph, GmemCompletion, GmemIssue, GmemReject, GmemRequest, GmemStats, GmemFlowConfig},
    graph::FlowGraph,
    icache::{IcacheFlowConfig, IcacheIssue, IcacheReject, IcacheRequest, IcacheStats, IcacheSubgraph},
    lsu::{LsuFlowConfig, LsuPayload, LsuCompletion, LsuIssue, LsuReject, LsuStats, LsuSubgraph},
    operand_fetch::{OperandFetchConfig, OperandFetchQueue, OperandFetchReject},
    smem::{SmemCompletion, SmemFlowConfig, SmemIssue, SmemReject, SmemRequest, SmemStats, SmemSubgraph, SmemUtilSample},
    tensor::{TensorConfig, TensorQueue, TensorReject},
    types::CoreFlowPayload,
    warp_scheduler::WarpSchedulerConfig,
    writeback::{WritebackConfig, WritebackIssue, WritebackPayload, WritebackQueue, WritebackReject, WritebackStats},
    execute::{ExecUnitKind, ExecutePipeline, ExecutePipelineConfig},
};
use crate::timeq::{Backpressure, Cycle, Ticket};
use serde::Deserialize;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CoreGraphConfig {
    #[serde(flatten)]
    pub memory: MemoryConfig,
    #[serde(flatten)]
    pub compute: ComputeConfig,
    #[serde(flatten)]
    pub io: IoConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub gmem: GmemFlowConfig,
    pub smem: SmemFlowConfig,
    pub lsu: LsuFlowConfig,
    pub icache: IcacheFlowConfig,
    pub writeback: WritebackConfig,
    pub operand_fetch: OperandFetchConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ComputeConfig {
    pub tensor: TensorConfig,
    pub scheduler: WarpSchedulerConfig,
    pub execute: ExecutePipelineConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IoConfig {
    pub barrier: BarrierConfig,
    pub fence: FenceConfig,
    pub dma: DmaConfig,
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
    cluster_gmem: Option<Arc<RwLock<ClusterGmemGraph>>>,
}

// Small macro to implement repeated indexed accessors for subgraphs
macro_rules! impl_indexed_accessor {
    ($ref_name:ident, $mut_name:ident, $with_name:ident, $idx_field:ident, $variant:path, $ty:ty, $panic_msg:expr) => {
        fn $ref_name(&self) -> &$ty {
            match &self.subgraphs[self.$idx_field] {
                $variant(val) => val,
                _ => unreachable!($panic_msg),
            }
        }

        fn $mut_name(&mut self) -> &mut $ty {
            match &mut self.subgraphs[self.$idx_field] {
                $variant(val) => val,
                _ => unreachable!($panic_msg),
            }
        }

        fn $with_name<R>(&mut self, f: impl FnOnce(&mut $ty) -> R) -> R {
            match &mut self.subgraphs[self.$idx_field] {
                $variant(val) => f(val),
                _ => unreachable!($panic_msg),
            }
        }
    };
}

#[allow(dead_code)]
impl CoreGraph {
    pub fn new(
        config: CoreGraphConfig,
        num_warps: usize,
        cluster_gmem: Option<Arc<RwLock<ClusterGmemGraph>>>,
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
        let cluster_gmem = cluster_gmem;

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
            cluster_gmem,
        }
    }

    pub fn issue_smem(
        &mut self,
        now: Cycle,
        request: SmemRequest,
    ) -> Result<SmemIssue, SmemReject> {
        self.with_smem_mut(|smem, graph| smem.issue(graph, now, request))
    }

    pub fn tick_front(&mut self, now: Cycle) {
        for subgraph in &mut self.subgraphs {
            subgraph.tick_phase(TickPhase::Front, now);
        }
        if let Some(cluster) = &mut self.cluster_gmem {
            cluster.write().unwrap().tick(now);
        }
    }

    pub fn tick_graph(&mut self, now: Cycle) {
        self.graph.tick(now);
        for subgraph in &mut self.subgraphs {
            subgraph.collect_completions(&mut self.graph, now);
        }
    }

    pub fn tick_back(&mut self, now: Cycle) {
        for subgraph in &mut self.subgraphs {
            subgraph.tick_phase(TickPhase::Back, now);
        }
    }

    pub fn collect_cluster_gmem_completions(&self, core_id: usize) -> Vec<GmemCompletion> {
        let Some(cluster) = &self.cluster_gmem else {
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
            Some(StatEnum::Smem(stats)) => stats,
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
            Some(StatEnum::Icache(stats)) => stats,
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

    pub fn barrier_stats(&self) -> BarrierSummary {
        self.barrier_ref().stats()
    }

    pub fn clear_barrier_stats(&mut self) {
        self.with_barrier_mut(|b| b.clear_stats());
    }

    pub fn cluster_gmem_issue(
        &self,
        core_id: usize,
        now: Cycle,
        request: GmemRequest,
    ) -> Result<GmemIssue, GmemReject> {
        let Some(cluster) = &self.cluster_gmem else {
            return Err(GmemReject::new(
                request,
                now.saturating_add(1),
                crate::timeflow::GmemRejectReason::QueueFull,
            ));
        };
        cluster.write().unwrap().issue(core_id, now, request)
    }

    pub fn cluster_gmem_stats(&self, core_id: usize) -> GmemStats {
        self.cluster_gmem
            .as_ref()
            .map(|cluster| cluster.read().unwrap().stats(core_id))
            .unwrap_or_default()
    }

    pub fn cluster_gmem_clear_stats(&self, core_id: usize) {
        if let Some(cluster) = &self.cluster_gmem {
            cluster.write().unwrap().clear_stats(core_id);
        }
    }

    pub fn cluster_gmem_hierarchy_stats_per_level(&self) -> (GmemStats, GmemStats, GmemStats) {
        self.cluster_gmem
            .as_ref()
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
            Some(StatEnum::Lsu(stats)) => stats,
            _ => unreachable!("lsu index always points to lsu"),
        }
    }

    pub fn clear_lsu_stats(&mut self) {
        self.subgraphs[self.lsu_index].clear_stats();
    }

    pub fn writeback_stats(&self) -> WritebackStats {
        match self.subgraphs[self.writeback_index].stats_snapshot() {
            Some(StatEnum::Writeback(stats)) => stats,
            _ => unreachable!("writeback index always points to writeback"),
        }
    }

    pub fn clear_writeback_stats(&mut self) {
        self.subgraphs[self.writeback_index].clear_stats();
    }

    pub fn sample_smem_utilization(&mut self) -> SmemUtilSample {
        self.with_smem_mut(|smem, graph| smem.sample_utilization(graph))
    }

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

    impl_indexed_accessor!(lsu_ref, lsu_mut, with_lsu_mut, lsu_index, CoreSubgraph::Lsu, LsuSubgraph, "lsu index always points to lsu");
    impl_indexed_accessor!(icache_ref, icache_mut, with_icache_mut, icache_index, CoreSubgraph::Icache, IcacheSubgraph, "icache index always points to icache");
    impl_indexed_accessor!(operand_fetch_ref, operand_fetch_mut, with_operand_fetch_mut, operand_fetch_index, CoreSubgraph::OperandFetch, OperandFetchQueue, "operand_fetch index always points to operand_fetch");
    impl_indexed_accessor!(dma_ref, dma_mut, with_dma_mut, dma_index, CoreSubgraph::Dma, DmaQueue, "dma index always points to dma");
    impl_indexed_accessor!(tensor_ref, tensor_mut, with_tensor_mut, tensor_index, CoreSubgraph::Tensor, TensorQueue, "tensor index always points to tensor");
    impl_indexed_accessor!(execute_ref, execute_mut, with_execute_mut, execute_index, CoreSubgraph::Execute, ExecutePipeline, "execute index always points to execute");
    impl_indexed_accessor!(writeback_ref, writeback_mut, with_writeback_mut, writeback_index, CoreSubgraph::Writeback, WritebackQueue, "writeback index always points to writeback");
    impl_indexed_accessor!(fence_ref, fence_mut, with_fence_mut, fence_index, CoreSubgraph::Fence, FenceQueue, "fence index always points to fence");
    impl_indexed_accessor!(barrier_ref, barrier_mut, with_barrier_mut, barrier_index, CoreSubgraph::Barrier, BarrierManager, "barrier index always points to barrier");

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
}


enum StatEnum {
    Smem(SmemStats),
    Icache(IcacheStats),
    Lsu(LsuStats),
    Writeback(WritebackStats),
}

#[derive(Debug, Clone, Copy)]
enum TickPhase {
    Front,
    Back,
}


trait Subgraph {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle);
    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle);
    fn stats_snapshot(&self) -> Option<StatEnum> {
        None
    }
    fn clear_stats(&mut self) {}
}

impl Subgraph for SmemSubgraph {
    fn tick_phase(&mut self, _phase: TickPhase, _now: Cycle) {}

    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        SmemSubgraph::collect_completions(self, graph, now);
    }

    fn stats_snapshot(&self) -> Option<StatEnum> {
        Some(StatEnum::Smem(self.stats.clone()))
    }

    fn clear_stats(&mut self) {
        self.stats = SmemStats::default();
    }
}

impl Subgraph for IcacheSubgraph {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}

    fn stats_snapshot(&self) -> Option<StatEnum> {
        Some(StatEnum::Icache(self.stats()))
    }

    fn clear_stats(&mut self) {
        IcacheSubgraph::clear_stats(self);
    }
}

impl Subgraph for LsuSubgraph {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}

    fn stats_snapshot(&self) -> Option<StatEnum> {
        Some(StatEnum::Lsu(self.stats()))
    }

    fn clear_stats(&mut self) {
        LsuSubgraph::clear_stats(self);
    }
}

impl Subgraph for OperandFetchQueue {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now, |_| {});
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}

impl Subgraph for DmaQueue {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}

impl Subgraph for TensorQueue {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}

impl Subgraph for ExecutePipeline {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Front) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}

impl Subgraph for WritebackQueue {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Back) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}

    fn stats_snapshot(&self) -> Option<StatEnum> {
        Some(StatEnum::Writeback(self.stats()))
    }

    fn clear_stats(&mut self) {
        WritebackQueue::clear_stats(self);
    }
}

impl Subgraph for FenceQueue {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        if matches!(phase, TickPhase::Back) {
            self.tick(now);
        }
    }

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}

impl Subgraph for BarrierManager {
    fn tick_phase(&mut self, _phase: TickPhase, _now: Cycle) {}

    fn collect_completions(&mut self, _graph: &mut FlowGraph<CoreFlowPayload>, _now: Cycle) {}
}



impl Subgraph for CoreSubgraph {
    fn tick_phase(&mut self, phase: TickPhase, now: Cycle) {
        match self {
            CoreSubgraph::Smem(smem) => smem.tick_phase(phase, now),
            CoreSubgraph::Icache(icache) => icache.tick_phase(phase, now),
            CoreSubgraph::Lsu(lsu) => lsu.tick_phase(phase, now),
            CoreSubgraph::OperandFetch(q) => q.tick_phase(phase, now),
            CoreSubgraph::Dma(dma) => dma.tick_phase(phase, now),
            CoreSubgraph::Tensor(tensor) => tensor.tick_phase(phase, now),
            CoreSubgraph::Execute(exec) => exec.tick_phase(phase, now),
            CoreSubgraph::Writeback(wb) => wb.tick_phase(phase, now),
            CoreSubgraph::Fence(fence) => fence.tick_phase(phase, now),
            CoreSubgraph::Barrier(barrier) => barrier.tick_phase(phase, now),
        }
    }

    fn collect_completions(&mut self, graph: &mut FlowGraph<CoreFlowPayload>, now: Cycle) {
        match self {
            CoreSubgraph::Smem(smem) => smem.collect_completions(graph, now),
            CoreSubgraph::Icache(icache) => icache.collect_completions(graph, now),
            CoreSubgraph::Lsu(lsu) => lsu.collect_completions(graph, now),
            CoreSubgraph::OperandFetch(q) => q.collect_completions(graph, now),
            CoreSubgraph::Dma(dma) => dma.collect_completions(graph, now),
            CoreSubgraph::Tensor(tensor) => tensor.collect_completions(graph, now),
            CoreSubgraph::Execute(exec) => exec.collect_completions(graph, now),
            CoreSubgraph::Writeback(wb) => wb.collect_completions(graph, now),
            CoreSubgraph::Fence(fence) => fence.collect_completions(graph, now),
            CoreSubgraph::Barrier(barrier) => barrier.collect_completions(graph, now),
        }
    } 

    fn stats_snapshot(&self) -> Option<StatEnum> {
        match self {
            CoreSubgraph::Smem(smem) => smem.stats_snapshot(),
            CoreSubgraph::Icache(icache) => icache.stats_snapshot(),
            CoreSubgraph::Lsu(lsu) => lsu.stats_snapshot(),
            CoreSubgraph::Writeback(wb) => wb.stats_snapshot(),
            _ => None,
        }
    }

    fn clear_stats(&mut self) {
        match self {
            CoreSubgraph::Smem(smem) => smem.clear_stats(),
            CoreSubgraph::Lsu(lsu) => lsu.clear_stats(),
            CoreSubgraph::Icache(icache) => icache.clear_stats(),
            CoreSubgraph::Writeback(wb) => wb.clear_stats(),
            _ => {}
        }
    }
}
