use super::*;
use crate::muon::config::{LaneConfig, MuonConfig};
use crate::muon::decode::IssuedInst;
use crate::muon::execute::Opcode;
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
    gmem.levels[0].tag.queue_capacity = 1;
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

fn make_model_with_execute(
    num_warps: usize,
    execute: crate::timeflow::ExecutePipelineConfig,
) -> CoreTimingModel {
    let mut cfg = CoreGraphConfig::default();
    cfg.compute.execute = execute;

    // Keep memory configs small but valid; GMEM is provided via the cluster graph.
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    CoreTimingModel::new(cfg, num_warps, 0, 0, cluster_gmem, logger)
}

fn issued_int_op() -> IssuedInst {
    IssuedInst {
        opcode: Opcode::OP_IMM,
        opext: 0,
        rd_addr: 1,
        f3: 0,
        rs1_addr: 2,
        rs2_addr: 3,
        rs3_addr: 0,
        rs4_addr: 0,
        rs1_data: vec![Some(1)],
        rs2_data: vec![Some(2)],
        rs3_data: vec![None],
        rs4_data: vec![None],
        f7: 0,
        imm32: 0,
        imm24: 0,
        csr_imm: 0,
        pc: 0,
        raw: 0,
    }
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

    let mut cfg = CoreGraphConfig::default();
    cfg.memory.gmem.policy.l0_enabled = false;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);
    let now = module_now(&scheduler);
    let mut request = GmemRequest::new(0, 16, 0x3, true);
    request.lane_addrs = Some(vec![0, 32]);
    model
        .issue_gmem_request(now, 0, request, &mut scheduler)
        .expect("coalesced request should accept");

    // With L0 disabled, coalescing should occur at the L1 line size (default 32B),
    // so 0 and 32 map to distinct lines and produce two pending splits.
    assert_eq!(model.outstanding_gmem(), 2);

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
fn gmem_coalescing_uses_l0_line_when_enabled() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut cfg = CoreGraphConfig::default();
    cfg.memory.gmem.policy.l0_enabled = true;
    cfg.memory.gmem.policy.l0_line_bytes = 64;
    cfg.memory.gmem.policy.l1_line_bytes = 32;

    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    let mut request = GmemRequest::new(0, 16, 0x3, true);
    request.lane_addrs = Some(vec![0, 32]);
    model
        .issue_gmem_request(now, 0, request, &mut scheduler)
        .expect("coalesced request should accept");

    // With L0 enabled and 64B lines, 0 and 32 coalesce into one request/split.
    assert_eq!(model.outstanding_gmem(), 1);
}

#[test]
fn execute_pipeline_stalls_until_ready() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let exec_cfg = crate::timeflow::ExecutePipelineConfig {
        alu: ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 64,
            queue_capacity: 4,
            ..ServerConfig::default()
        },
        ..Default::default()
    };
    let mut model = make_model_with_execute(1, exec_cfg);
    let inst = issued_int_op();

    let now = module_now(&scheduler);
    let wait_until = model
        .issue_execute(now, 0, &inst, 32, &mut scheduler)
        .expect_err("execute should stall when latency > 0");
    assert_eq!(now + 3, wait_until);

    // Not ready yet => still stalls.
    model.tick(now + 1, &mut scheduler);
    assert!(model
        .issue_execute(now + 1, 0, &inst, 32, &mut scheduler)
        .is_err());

    // At ready => completes and allows execution.
    model.tick(wait_until, &mut scheduler);
    assert!(model
        .issue_execute(wait_until, 0, &inst, 32, &mut scheduler)
        .is_ok());
}

#[test]
fn execute_utilization_counts_busy_cycles() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let exec_cfg = crate::timeflow::ExecutePipelineConfig {
        alu: ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 64,
            queue_capacity: 4,
            ..ServerConfig::default()
        },
        ..Default::default()
    };
    let mut model = make_model_with_execute(1, exec_cfg);
    let inst = issued_int_op();

    let now = module_now(&scheduler);
    let wait_until = model
        .issue_execute(now, 0, &inst, 32, &mut scheduler)
        .expect_err("execute should stall");

    // Drive until completion; utilization should have recorded at least one busy sample.
    for cycle in now..=wait_until {
        model.tick(cycle, &mut scheduler);
    }
    let summary = model.perf_summary();
    assert!(summary.execute_util.cycles > 0);
    assert!(summary.execute_util.int_busy_sum > 0);
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
fn perf_summary_tracks_dma_bytes() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut cfg = CoreGraphConfig::default();
    cfg.io.dma.enabled = true;
    cfg.io.dma.queue.base_latency = 1;
    cfg.io.dma.queue.bytes_per_cycle = 16;
    cfg.io.dma.queue.queue_capacity = 2;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    let ticket = model.issue_dma(now, 128).expect("dma issue");
    for cycle in now..=ticket.ready_at() {
        model.tick(cycle, &mut scheduler);
    }

    let summary = model.perf_summary();
    assert!(summary.dma_bytes_issued >= 128);
    assert!(summary.dma_bytes_completed >= 128);
}

#[test]
fn perf_summary_tracks_tensor_bytes() {
    let mut scheduler = make_scheduler(1);
    scheduler.spawn_single_warp();

    let mut cfg = CoreGraphConfig::default();
    cfg.compute.tensor.enabled = true;
    cfg.compute.tensor.queue.base_latency = 1;
    cfg.compute.tensor.queue.bytes_per_cycle = 16;
    cfg.compute.tensor.queue.queue_capacity = 2;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);

    let now = module_now(&scheduler);
    let ticket = model.issue_tensor(now, 64).expect("tensor issue");
    for cycle in now..=ticket.ready_at() {
        model.tick(cycle, &mut scheduler);
    }

    let summary = model.perf_summary();
    assert!(summary.tensor_bytes_issued >= 64);
    assert!(summary.tensor_bytes_completed >= 64);
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

    let mut cfg = CoreGraphConfig::default();
    cfg.memory.gmem.policy.l0_enabled = false;
    let logger = Arc::new(Logger::silent());
    let cluster_gmem = Arc::new(std::sync::RwLock::new(ClusterGmemGraph::new(
        cfg.memory.gmem.clone(),
        1,
        1,
    )));
    let mut model = CoreTimingModel::new(cfg, 1, 0, 0, cluster_gmem, logger);
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
    assert_eq!(summary.gmem_hits.l0_accesses, 0);
    assert_eq!(summary.gmem_hits.l0_hits, 0);
    assert_eq!(summary.gmem_hits.l1_accesses, 2);
    assert_eq!(summary.gmem_hits.l1_hits, 1);
}
