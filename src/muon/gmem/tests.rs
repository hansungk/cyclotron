use super::*;
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::scheduler::Scheduler;
use crate::sim::log::Logger;
use crate::timeflow::{ClusterGmemGraph, CoreGraphConfig, GmemFlowConfig, SmemFlowConfig};
use crate::timeq::{module_now, ServerConfig};
use std::sync::Arc;

fn make_scheduler(num_warps: usize) -> Scheduler {
    let mut config = MuonConfig::default();
    config.num_warps = num_warps;
    config.lane_config = LaneConfig::default();
    Scheduler::new(Arc::new(config), 0)
}

fn make_model_with_lsu(num_warps: usize, lsu_depth: usize) -> CoreTimingModel {
    let mut gmem = GmemFlowConfig::default();
    gmem.nodes.coalescer.queue_capacity = 1;
    gmem.nodes.l0d_tag.queue_capacity = 1;
    gmem.nodes.dram.queue_capacity = 1;
    gmem.links.default.entries = 1;
    let smem = SmemFlowConfig {
        lane: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        serial: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        crossbar: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        subbank: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        bank: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        dual_port: false,
        num_banks: 1,
        num_lanes: 1,
        num_subbanks: 1,
        word_bytes: 4,
        serialize_cores: false,
        link_capacity: 1,
        smem_log_period: 1000,
    };
    let mut cfg = CoreGraphConfig::default();
    cfg.memory.gmem = gmem;
    cfg.memory.smem = smem;
    cfg.memory.lsu.queues.global_ldq.queue_capacity = lsu_depth.max(1);
    cfg.memory.lsu.queues.global_stq.queue_capacity = lsu_depth.max(1);
    cfg.memory.lsu.queues.shared_ldq.queue_capacity = lsu_depth.max(1);
    cfg.memory.lsu.queues.shared_stq.queue_capacity = lsu_depth.max(1);
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    CoreTimingModel::new(cfg, num_warps, 0, 0, cluster_gmem, logger)
}

fn make_model(num_warps: usize) -> CoreTimingModel {
    make_model_with_lsu(num_warps, 8)
}

#[test]
fn accepted_request_marks_pending_until_completion() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut model = make_model(1);
    let now = module_now(&scheduler);
    let request = GmemRequest::new(0, 16, 0xF, true);
    let ticket = model
        .issue_gmem_request(now, 0, request, &mut scheduler)
        .expect("request should accept");

    let ready_at = ticket.ready_at();
    let mut cycle = now;
    let mut completed = false;
    let max_cycles = 500;
    for _ in 0..max_cycles {
        model.tick(cycle, &mut scheduler);
        if model.stats().gmem.completed() >= 1 {
            completed = true;
            break;
        }
        cycle = cycle.saturating_add(1);
    }

    assert!(
        completed,
        "GMEM request did not complete within {} cycles (ready_at={}, outstanding={})",
        max_cycles,
        ready_at,
        model.outstanding_gmem()
    );
    assert!(!model.has_pending_gmem(0));
    assert_eq!(model.stats().gmem.completed(), 1);
    assert!(model.stats().gmem.issued() >= 1);
}

#[test]
fn gmem_coalescing_adds_multiple_pending_entries() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut model = make_model(1);
    let now = module_now(&scheduler);
    let mut request = GmemRequest::new(0, 16, 0x3, true);
    request.lane_addrs = Some(vec![0, 32]);
    model
        .issue_gmem_request(now, 0, request, &mut scheduler)
        .expect("coalesced request should accept");

    assert_eq!(model.outstanding_gmem(), 1);

    let max_cycles = 1000;
    let mut cycle = now;
    let mut completed = false;
    for _ in 0..max_cycles {
        model.tick(cycle, &mut scheduler);
        if model.outstanding_gmem() == 0 {
            completed = true;
            break;
        }
        cycle = cycle.saturating_add(1);
    }

    assert!(
        completed,
        "coalesced GMEM request did not complete within {} cycles (outstanding={})",
        max_cycles,
        model.outstanding_gmem()
    );
}

#[test]
fn queue_full_schedules_retry_and_replay() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut model = make_model_with_lsu(1, 1);
    let now = module_now(&scheduler);
    let request0 = GmemRequest::new(0, 16, 0xF, true);
    model
        .issue_gmem_request(now, 0, request0, &mut scheduler)
        .expect("first request should accept");

    let mut request1 = GmemRequest::new(0, 16, 0xF, true);
    request1.addr = 128;
    let retry_at = model
        .issue_gmem_request(now, 0, request1, &mut scheduler)
        .expect_err("second request should stall on LSU queue");
    assert!(retry_at > now);
    assert!(scheduler.get_schedule(0).is_none());
}

#[test]
fn smem_request_tracks_pending_and_stats() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut model = make_model(1);
    let now = module_now(&scheduler);
    let request = SmemRequest::new(0, 32, 0xF, false, 0);
    let ticket = model
        .issue_smem_request(now, 0, request, &mut scheduler)
        .expect("smem request should accept");

    let ready_at = ticket.ready_at();
    let mut cycle = now;
    let mut completed = false;
    for _ in 0..200 {
        model.tick(cycle, &mut scheduler);
        if model.stats().smem.completed >= 1 {
            completed = true;
            break;
        }
        cycle = cycle.saturating_add(1);
    }

    assert!(
        completed,
        "SMEM request did not complete within 200 cycles (ready_at={}, outstanding={})",
        ready_at,
        model.outstanding_smem()
    );
    let stats = model.stats();
    assert_eq!(stats.smem.completed, 1);
}

#[test]
fn smem_crossbar_capacity_backpressures_new_requests() {
    let mut scheduler = make_scheduler(2);
    let threads = vec![vec![(0, 0, 0)], vec![(0, 0, 1)]];
    scheduler.spawn_n_warps(0x8000_0000, &threads);

    let gmem = GmemFlowConfig::default();
    let smem = SmemFlowConfig {
        lane: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        serial: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        crossbar: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        subbank: ServerConfig {
            queue_capacity: 1,
            ..ServerConfig::default()
        },
        bank: ServerConfig {
            queue_capacity: 2,
            ..ServerConfig::default()
        },
        dual_port: false,
        num_banks: 2,
        num_lanes: 1,
        num_subbanks: 1,
        word_bytes: 4,
        serialize_cores: false,
        link_capacity: 1,
        smem_log_period: 1000,
    };
    let mut cfg = CoreGraphConfig::default();
    cfg.memory.gmem = gmem;
    cfg.memory.smem = smem;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 2, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    let req0 = SmemRequest::new(0, 32, 0xF, false, 0);
    model
        .issue_smem_request(now, 0, req0, &mut scheduler)
        .expect("first SMEM request should be accepted");

    let req1 = SmemRequest::new(1, 32, 0xF, false, 1);
    model
        .issue_smem_request(now, 1, req1, &mut scheduler)
        .expect("second SMEM request should be accepted into LSU");

    for cycle in now..now + 50 {
        model.tick(cycle, &mut scheduler);
    }

    let stats = model.stats().smem;
    assert!(
        stats.queue_full_rejects + stats.busy_rejects >= 1,
        "expected at least one rejection to be counted (queue_full={}, busy={})",
        stats.queue_full_rejects,
        stats.busy_rejects
    );
}

#[test]
fn mmio_store_triggers_dma_queue() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut cfg = CoreGraphConfig::default();
    cfg.io.dma.enabled = true;
    cfg.io.dma.mmio_base = 0x1000;
    cfg.io.dma.mmio_size = 0x100;
    cfg.io.dma.queue.base_latency = 1;
    cfg.memory.lsu.queues.global_ldq.queue_capacity = 8;
    cfg.memory.lsu.queues.global_stq.queue_capacity = 8;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    let mut request = GmemRequest::new(0, 4, 0x1, false);
    request.addr = 0x1000;
    model
        .issue_gmem_request(now, 0, request, &mut scheduler)
        .expect("mmio store should accept");

    for cycle in now..now + 5 {
        model.tick(cycle, &mut scheduler);
    }

    assert!(model.dma_completed() >= 1);
}

#[test]
fn csr_write_triggers_tensor_queue() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut cfg = CoreGraphConfig::default();
    cfg.compute.tensor.enabled = true;
    cfg.compute.tensor.csr_addrs = vec![0x7c0];
    cfg.compute.tensor.queue.base_latency = 1;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    model.notify_csr_write(now, 0x7c0);
    for cycle in now..now + 5 {
        model.tick(cycle, &mut scheduler);
    }

    assert!(model.tensor_completed() >= 1);
}

#[test]
fn sequential_loads_benefit_from_cache() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut model = make_model(1);
    let now = module_now(&scheduler);

    let mut req0 = GmemRequest::new(0, 16, 0xF, true);
    req0.addr = 0x1000;
    model
        .issue_gmem_request(now, 0, req0, &mut scheduler)
        .expect("first request should accept");

    let mut cycle = now;
    while model.stats().gmem.completed() == 0 {
        model.tick(cycle, &mut scheduler);
        cycle = cycle.saturating_add(1);
    }

    let mut req1 = GmemRequest::new(0, 16, 0xF, true);
    req1.addr = 0x1004;
    model
        .issue_gmem_request(cycle, 0, req1, &mut scheduler)
        .expect("second request should accept");

    while model.stats().gmem.completed() < 2 {
        model.tick(cycle, &mut scheduler);
        cycle = cycle.saturating_add(1);
    }

    let summary = model.perf_summary();
    assert_eq!(summary.gmem_hits.l0_accesses, 2);
    assert_eq!(summary.gmem_hits.l0_hits, 1);
}
