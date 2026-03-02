use crate::timeflow::smem::{SmemFlowConfig, SmemRejectReason, SmemRequest, SmemSubgraph};
use crate::timeflow::{CoreFlowPayload, FlowGraph};
use crate::timeq::Cycle;

fn drive_until_ready(
    graph: &mut FlowGraph<CoreFlowPayload>,
    subgraph: &mut SmemSubgraph,
    ready_at: Cycle,
    extra: Cycle,
) {
    for cycle in 0..=ready_at.saturating_add(extra) {
        graph.tick(cycle);
        subgraph.collect_completions(graph, cycle);
    }
}

#[test]
fn smem_requests_complete() {
    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &SmemFlowConfig::default());
    let req = SmemRequest::new(0, 32, 0xF, false, 0);
    let issue = subgraph
        .issue(&mut graph, 0, req)
        .expect("smem issue should succeed");
    let ready_at = issue.ticket.ready_at();
    drive_until_ready(&mut graph, &mut subgraph, ready_at, 10);
    assert_eq!(1, subgraph.completions.len());
}

#[test]
fn smem_backpressure_is_reported() {
    let mut cfg = SmemFlowConfig::default();
    cfg.bank.queue_capacity = 1;
    cfg.crossbar.queue_capacity = 1;
    cfg.lane.queue_capacity = 1;
    cfg.num_lanes = 1;
    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);
    let req0 = SmemRequest::new(0, 32, 0xF, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();
    let req1 = SmemRequest::new(0, 32, 0xF, false, 0);
    let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
    assert_eq!(SmemRejectReason::QueueFull, err.reason);
}

#[test]
fn smem_stats_track_activity() {
    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &SmemFlowConfig::default());
    let req = SmemRequest::new(0, 32, 0xF, false, 0);
    let issue = subgraph.issue(&mut graph, 0, req).unwrap();
    let ready_at = issue.ticket.ready_at();
    drive_until_ready(&mut graph, &mut subgraph, ready_at, 100);
    let stats = subgraph.stats;
    assert_eq!(1, stats.issued);
    assert_eq!(1, stats.completed);
    assert_eq!(32, stats.bytes_issued);
    assert_eq!(32, stats.bytes_completed);
}

#[test]
fn smem_serialization_backpressures_second_request() {
    let mut cfg = SmemFlowConfig::default();
    cfg.serialize_cores = true;
    cfg.serial.queue_capacity = 1;
    cfg.num_lanes = 1;
    cfg.num_banks = 1;
    cfg.num_subbanks = 1;
    cfg.lane.queue_capacity = 4;
    cfg.crossbar.queue_capacity = 4;
    cfg.bank.queue_capacity = 4;
    cfg.subbank.queue_capacity = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);
    let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();
    let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
    assert_eq!(SmemRejectReason::QueueFull, err.reason);
}

#[test]
fn smem_subbank_conflict_serializes() {
    let mut cfg = SmemFlowConfig::default();
    cfg.serialize_cores = false;
    cfg.num_lanes = 1;
    cfg.num_banks = 1;
    cfg.num_subbanks = 2;
    cfg.lane.queue_capacity = 4;
    cfg.crossbar.queue_capacity = 4;
    cfg.subbank.queue_capacity = 1;
    cfg.subbank.base_latency = 1;
    cfg.bank.queue_capacity = 4;
    cfg.bank.base_latency = 0;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let mut req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    req0.subbank = 0;
    let mut req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    req1.subbank = 0;

    subgraph.issue(&mut graph, 0, req0).unwrap();
    subgraph.issue(&mut graph, 0, req1).unwrap();

    let mut completions = Vec::new();
    for cycle in 0..20 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completions.push(done);
        }
    }

    assert_eq!(completions.len(), 2);
    assert!(completions[1].completed_at > completions[0].completed_at);
}

#[test]
fn smem_single_bank_configuration() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_banks = 1;
    cfg.num_lanes = 2;
    cfg.num_subbanks = 1;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();
    subgraph.issue(&mut graph, 0, req1).unwrap();

    let mut count = 0;
    for cycle in 0..50 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        count += subgraph.completions.len();
        subgraph.completions.clear();
        if count == 2 {
            return;
        }
    }
    panic!("expected completions for both requests");
}

#[test]
fn smem_single_lane_configuration() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_lanes = 1;
    cfg.num_banks = 2;
    cfg.num_subbanks = 1;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let req = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req).unwrap();
    for cycle in 0..50 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        if !subgraph.completions.is_empty() {
            return;
        }
    }
    panic!("expected completion in single-lane config");
}

#[test]
fn smem_single_subbank_configuration() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_lanes = 1;
    cfg.num_banks = 2;
    cfg.num_subbanks = 1;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let req = SmemRequest::new(0, 16, 0x1, false, 1);
    subgraph.issue(&mut graph, 0, req).unwrap();
    for cycle in 0..50 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        if !subgraph.completions.is_empty() {
            return;
        }
    }
    panic!("expected completion in single-subbank config");
}

#[test]
fn smem_address_zero_access() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_lanes = 1;
    cfg.num_banks = 2;
    cfg.num_subbanks = 2;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let mut req = SmemRequest::new(0, 16, 0x1, false, 0);
    req.addr = 0;
    subgraph.issue(&mut graph, 0, req).unwrap();
    for cycle in 0..50 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        if !subgraph.completions.is_empty() {
            return;
        }
    }
    panic!("expected completion for address 0");
}

#[test]
fn smem_dual_port_allows_parallel_read_write() {
    let mut cfg = SmemFlowConfig::default();
    cfg.dual_port = true;
    cfg.num_lanes = 1;
    cfg.num_banks = 1;
    cfg.num_subbanks = 1;
    cfg.lane.base_latency = 0;
    cfg.crossbar.base_latency = 0;
    cfg.subbank.base_latency = 0;
    cfg.bank.base_latency = 1;
    cfg.bank.queue_capacity = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let read = SmemRequest::new(0, 16, 0x1, false, 0);
    let write = SmemRequest::new(0, 16, 0x1, true, 0);
    subgraph.issue(&mut graph, 0, read).unwrap();
    subgraph.issue(&mut graph, 0, write).unwrap();

    let mut completions = Vec::new();
    for cycle in 0..10 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completions.push(done);
        }
        if completions.len() == 2 {
            break;
        }
    }
    assert_eq!(completions.len(), 2);
    let min = completions[0].completed_at.min(completions[1].completed_at);
    let max = completions[0].completed_at.max(completions[1].completed_at);
    assert!(
        max - min <= 1,
        "dual-port should allow near-parallel completion"
    );
}

#[test]
fn smem_dual_port_disabled_serializes() {
    let mut cfg = SmemFlowConfig::default();
    cfg.dual_port = false;
    cfg.num_lanes = 1;
    cfg.num_banks = 1;
    cfg.num_subbanks = 1;
    cfg.lane.base_latency = 0;
    cfg.crossbar.base_latency = 0;
    cfg.subbank.base_latency = 0;
    cfg.bank.base_latency = 1;
    cfg.bank.queue_capacity = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let read = SmemRequest::new(0, 16, 0x1, false, 0);
    let write = SmemRequest::new(0, 16, 0x1, true, 0);
    subgraph.issue(&mut graph, 0, read).unwrap();
    subgraph.issue(&mut graph, 0, write).unwrap();

    let mut completions = Vec::new();
    for cycle in 0..10 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completions.push(done);
        }
        if completions.len() == 2 {
            break;
        }
    }
    assert_eq!(completions.len(), 2);
    assert!(
        completions[1].completed_at > completions[0].completed_at,
        "single-port should serialize read/write"
    );
}

#[test]
fn smem_all_lanes_same_bank_maximum_conflict() {
    let mut cfg = SmemFlowConfig::default();
    cfg.dual_port = false;
    cfg.num_lanes = 16;
    cfg.num_banks = 1;
    cfg.num_subbanks = 1;
    cfg.lane.base_latency = 0;
    cfg.crossbar.base_latency = 0;
    cfg.subbank.base_latency = 0;
    cfg.bank.base_latency = 1;
    cfg.bank.queue_capacity = 32;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    for warp in 0..16 {
        let req = SmemRequest::new(warp, 16, 0x1, false, 0);
        subgraph.issue(&mut graph, 0, req).unwrap();
    }

    let mut completed_at = Vec::new();
    for cycle in 0..100 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completed_at.push(done.completed_at);
        }
        if completed_at.len() == 16 {
            break;
        }
    }
    assert_eq!(completed_at.len(), 16);
    completed_at.sort();
    assert!(
        completed_at.last().unwrap() - completed_at.first().unwrap() >= 15,
        "expected strong serialization under bank conflict"
    );
}

#[test]
fn smem_all_lanes_different_banks_no_conflict() {
    let mut cfg = SmemFlowConfig::default();
    cfg.dual_port = false;
    cfg.num_lanes = 16;
    cfg.num_banks = 16;
    cfg.num_subbanks = 1;
    cfg.lane.base_latency = 0;
    cfg.crossbar.base_latency = 0;
    cfg.subbank.base_latency = 0;
    cfg.bank.base_latency = 1;
    cfg.bank.queue_capacity = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    for warp in 0..16 {
        let req = SmemRequest::new(warp, 16, 0x1, false, warp);
        subgraph.issue(&mut graph, 0, req).unwrap();
    }

    let mut completed_at = Vec::new();
    for cycle in 0..20 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completed_at.push(done.completed_at);
        }
        if completed_at.len() == 16 {
            break;
        }
    }
    assert_eq!(completed_at.len(), 16);
    let min = *completed_at.iter().min().unwrap();
    let max = *completed_at.iter().max().unwrap();
    assert!(
        max - min <= 1,
        "expected near-parallel completion when banks differ"
    );
}
