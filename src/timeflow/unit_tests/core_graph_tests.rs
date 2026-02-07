use crate::timeflow::core_graph::CoreGraphConfig;
use crate::timeflow::{
    CoreGraph, ExecUnitKind, FenceRequest, GmemCompletion, GmemRequest, IcacheRequest,
    SmemRequest, WritebackPayload,
};

fn zero_smem_latency(cfg: &mut CoreGraphConfig) {
    cfg.memory.smem.lane.base_latency = 0;
    cfg.memory.smem.crossbar.base_latency = 0;
    cfg.memory.smem.subbank.base_latency = 0;
    cfg.memory.smem.bank.base_latency = 0;
}

fn core_graph_with_cfg<F>(num_warps: usize, use_cluster: bool, mut f: F) -> CoreGraph
where
    F: FnMut(&mut CoreGraphConfig),
{
    let mut cfg = CoreGraphConfig::default();
    f(&mut cfg);
    let cluster = if use_cluster {
        Some(std::sync::Arc::new(std::sync::RwLock::new(
            crate::timeflow::gmem::ClusterGmemGraph::new(cfg.memory.gmem.clone(), 1, 1),
        )))
    } else {
        None
    };
    CoreGraph::new(cfg, num_warps, cluster)
}

#[test]
fn core_graph_completes_smem_requests() {
    let mut graph = core_graph_with_cfg(1, false, zero_smem_latency);
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
    let mut graph = core_graph_with_cfg(1, false, zero_smem_latency);
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
    let mut graph = core_graph_with_cfg(1, false, |cfg| {
        cfg.memory.icache.policy.hit_rate = 1.0;
        cfg.memory.icache.hit.base_latency = 0;
        cfg.memory.icache.hit.queue_capacity = 2;

        cfg.memory.lsu.issue.base_latency = 0;
        cfg.memory.lsu.issue.queue_capacity = 2;
        cfg.memory.lsu.queues.global_ldq.queue_capacity = 2;
    });
    let icache_req = IcacheRequest::new(0, 0x100, 8);
    graph.issue_icache(0, icache_req).expect("icache issue");

    let mut gmem_req = GmemRequest::new(0, 16, 0xF, true);
    gmem_req.addr = 0x200;
    graph.lsu_issue_gmem(0, gmem_req).expect("lsu issue");

    let mut saw_ready = false;
    for cycle in 0..50 {
        graph.tick_front(cycle);
        if graph.lsu_peek_ready(cycle).is_some() {
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
    let mut graph = core_graph_with_cfg(1, true, |cfg| {
        cfg.memory.gmem.policy.l0_enabled = false;
        cfg.memory.gmem.nodes.dram.queue_capacity = 4;
        cfg.memory.gmem.nodes.dram.base_latency = 1;
    });

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
    let mut graph = core_graph_with_cfg(1, false, |cfg| {
        cfg.memory.writeback.enabled = true;
        cfg.memory.writeback.queue.base_latency = 1;
        cfg.memory.writeback.queue.queue_capacity = 2;
        cfg.io.fence.enabled = true;
        cfg.io.fence.queue.base_latency = 1;
        cfg.io.fence.queue.queue_capacity = 2;
    });
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
fn core_graph_ticks_dma_and_tensor_in_front_phase() {
    let mut graph = core_graph_with_cfg(1, false, |cfg| {
        cfg.io.dma.enabled = true;
        cfg.io.dma.queue.base_latency = 1;
        cfg.io.dma.queue.queue_capacity = 2;
        cfg.compute.tensor.enabled = true;
        cfg.compute.tensor.queue.base_latency = 1;
        cfg.compute.tensor.queue.queue_capacity = 2;
    });
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
    let mut graph = core_graph_with_cfg(1, false, |cfg| {
        cfg.compute.execute.alu.base_latency = 1;
        cfg.compute.execute.alu.queue_capacity = 1;
        cfg.compute.execute.alu.bytes_per_cycle = 1;
    });
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
    let mut graph = core_graph_with_cfg(1, true, |cfg| {
        zero_smem_latency(cfg);
        cfg.memory.gmem.nodes.dram.queue_capacity = 4;
        cfg.memory.gmem.nodes.dram.base_latency = 1;
        cfg.memory.gmem.policy.l0_enabled = false;
    });

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
