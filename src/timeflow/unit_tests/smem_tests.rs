use crate::timeflow::smem::{SmemFlowConfig, SmemRejectReason, SmemRequest, SmemSubgraph};
use crate::timeflow::{CoreFlowPayload, FlowGraph};
use crate::timeq::Cycle;

fn issue_with_retry(
    graph: &mut FlowGraph<CoreFlowPayload>,
    subgraph: &mut SmemSubgraph,
    mut request: SmemRequest,
    start: Cycle,
    deadline: Cycle,
) -> Cycle {
    for cycle in start..=deadline {
        graph.tick(cycle);
        subgraph.collect_completions(graph, cycle);
        match subgraph.issue(graph, cycle, request) {
            Ok(_) => return cycle,
            Err(reject) => {
                request = reject.payload;
            }
        }
    }
    panic!(
        "issue_with_retry: request not accepted by cycle {}",
        deadline
    );
}

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
    drive_until_ready(&mut graph, &mut subgraph, ready_at, 100);
    assert_eq!(1, subgraph.completions.len());
}

#[test]
fn smem_backpressure_is_reported() {
    let mut cfg = SmemFlowConfig::default();
    cfg.max_outstanding = 1;
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
    let stats = &subgraph.stats;
    assert_eq!(1, stats.issued);
    assert_eq!(1, stats.completed);
    assert_eq!(32, stats.bytes_issued);
    assert_eq!(32, stats.bytes_completed);
}

#[test]
fn smem_serialization_backpressures_second_request() {
    let mut cfg = SmemFlowConfig::default();
    cfg.max_outstanding = 1;
    cfg.num_subbanks = 1;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);
    let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();
    let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    let err = subgraph.issue(&mut graph, 0, req1).unwrap_err();
    assert_eq!(SmemRejectReason::QueueFull, err.reason);
}

#[test]
fn smem_single_wave_conflict_serializes() {
    // Two separate single-wave requests issued sequentially should
    // complete at different times.
    let mut cfg = SmemFlowConfig::default();
    cfg.num_subbanks = 2;
    cfg.max_outstanding = 8;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();

    let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    issue_with_retry(&mut graph, &mut subgraph, req1, 1, 20);

    let mut completions = Vec::new();
    for cycle in 0..50 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        while let Some(done) = subgraph.completions.pop_front() {
            completions.push(done);
        }
    }

    assert_eq!(completions.len(), 2);
    assert!(
        completions[1].completed_at > completions[0].completed_at,
        "second request should complete after first"
    );
}

#[test]
fn smem_single_bank_configuration() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_banks = 1;
    cfg.num_lanes = 2;
    cfg.num_subbanks = 1;
    cfg.max_outstanding = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let req0 = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, req0).unwrap();

    let req1 = SmemRequest::new(1, 16, 0x1, false, 0);
    issue_with_retry(&mut graph, &mut subgraph, req1, 1, 30);

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
fn smem_read_write_different_subbanks() {
    // Read and write to different subbanks should complete close together
    let mut cfg = SmemFlowConfig::default();
    cfg.num_subbanks = 2;
    cfg.max_outstanding = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let mut read = SmemRequest::new(0, 16, 0x1, false, 0);
    read.lane_addrs = Some(vec![0]); // sb0
    subgraph.issue(&mut graph, 0, read).unwrap();

    let mut write = SmemRequest::new(0, 16, 0x1, true, 0);
    write.lane_addrs = Some(vec![4]); // sb1
    issue_with_retry(&mut graph, &mut subgraph, write, 1, 10);

    let mut completions = Vec::new();
    for cycle in 0..20 {
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
        max - min <= 2,
        "parallel read/write to different subbanks should have close completions, got {} and {}",
        min,
        max
    );
}

#[test]
fn smem_same_subbank_serializes() {
    // Two requests issued at different cycles should complete in order
    let mut cfg = SmemFlowConfig::default();
    cfg.num_subbanks = 1;
    cfg.max_outstanding = 4;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let read = SmemRequest::new(0, 16, 0x1, false, 0);
    subgraph.issue(&mut graph, 0, read).unwrap();

    let write = SmemRequest::new(0, 16, 0x1, true, 0);
    issue_with_retry(&mut graph, &mut subgraph, write, 1, 10);

    let mut completions = Vec::new();
    for cycle in 0..20 {
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
        completions[1].completed_at >= completions[0].completed_at,
        "second request should complete at or after first"
    );
}

#[test]
fn smem_multiple_requests_same_subbank() {
    // 16 separate requests all to same subbank -> serialization
    let mut cfg = SmemFlowConfig::default();
    cfg.num_banks = 1;
    cfg.num_subbanks = 1;
    cfg.max_outstanding = 32;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let mut issued = 0;
    for cycle in 0..100u64 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        if issued < 16 {
            let req = SmemRequest::new(issued, 16, 0x1, false, 0);
            match subgraph.issue(&mut graph, cycle, req) {
                Ok(_) => {
                    issued += 1;
                }
                Err(_) => {}
            }
        }
    }

    let mut completed_at: Vec<Cycle> = Vec::new();
    for cycle in 0..200u64 {
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
        completed_at.last().unwrap() - completed_at.first().unwrap() >= 10,
        "expected strong serialization under bank conflict, got spread={}",
        completed_at.last().unwrap() - completed_at.first().unwrap()
    );
}

#[test]
fn smem_different_subbanks_no_conflict() {
    // 16 requests to different subbanks -> close completion times
    let mut cfg = SmemFlowConfig::default();
    cfg.num_subbanks = 16;
    cfg.max_outstanding = 32;

    let mut graph: FlowGraph<CoreFlowPayload> = FlowGraph::new();
    let mut subgraph = SmemSubgraph::attach(&mut graph, &cfg);

    let mut issued = 0;
    for cycle in 0..100u64 {
        graph.tick(cycle);
        subgraph.collect_completions(&mut graph, cycle);
        if issued < 16 {
            let mut req = SmemRequest::new(issued, 16, 0x1, false, issued);
            req.lane_addrs = Some(vec![(issued as u64) * 4]); // different subbanks
            match subgraph.issue(&mut graph, cycle, req) {
                Ok(_) => {
                    issued += 1;
                }
                Err(_) => {}
            }
        }
    }

    let mut completed_at: Vec<Cycle> = Vec::new();
    for cycle in 0..200u64 {
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
    let spread = completed_at.last().unwrap() - completed_at.first().unwrap();
    assert!(
        spread <= 20,
        "expected low spread when subbanks differ, got {}",
        spread
    );
}

#[test]
fn simulate_pattern_timed_all_coalesced() {
    let cfg = SmemFlowConfig::default();
    let subgraph = SmemSubgraph::attach(&mut FlowGraph::new(), &cfg);

    let n_waves = 100;
    let addrs: Vec<Vec<u64>> = (0..n_waves).map(|_| vec![0u64; cfg.num_lanes]).collect();
    let duration = subgraph.simulate_pattern_timed(&addrs, false, 4, 1, 0);
    assert!(duration > 0);
}

#[test]
fn simulate_pattern_timed_full_conflict_slower_than_coalesced() {
    let mut cfg = SmemFlowConfig::default();
    cfg.num_subbanks = 1;
    let subgraph = SmemSubgraph::attach(&mut FlowGraph::new(), &cfg);

    let n_waves = 10;
    let addrs_coalesced: Vec<Vec<u64>> = (0..n_waves).map(|_| vec![0u64; cfg.num_lanes]).collect();
    let duration_coalesced = subgraph.simulate_pattern_timed(&addrs_coalesced, false, 4, 1, 0);

    let addrs_conflict: Vec<Vec<u64>> = (0..n_waves)
        .map(|w| {
            (0..cfg.num_lanes)
                .map(|l| ((w * cfg.num_lanes + l) as u64) * 4)
                .collect()
        })
        .collect();
    let duration_conflict = subgraph.simulate_pattern_timed(&addrs_conflict, false, 4, 1, 0);
    assert!(duration_conflict >= duration_coalesced);
}

#[test]
fn simulate_pattern_timed_read_is_not_faster_than_write() {
    let cfg = SmemFlowConfig::default();
    let subgraph = SmemSubgraph::attach(&mut FlowGraph::new(), &cfg);

    let addrs: Vec<Vec<u64>> = (0..100)
        .map(|_| (0..cfg.num_lanes).map(|l| (l as u64) * 4).collect())
        .collect();
    let dur_write = subgraph.simulate_pattern_timed(&addrs, false, 4, 2, 0);
    let dur_read = subgraph.simulate_pattern_timed(&addrs, true, 4, 2, 0);
    assert!(dur_read >= dur_write);
}

#[test]
fn simulate_pattern_timed_issue_gap_throttles_traffic() {
    let cfg = SmemFlowConfig::default();
    let subgraph = SmemSubgraph::attach(&mut FlowGraph::new(), &cfg);

    let addrs: Vec<Vec<u64>> = (0..64)
        .map(|_| (0..cfg.num_lanes).map(|l| (l as u64) * 4).collect())
        .collect();
    let no_gap = subgraph.simulate_pattern_timed(&addrs, false, 4, 8, 0);
    let gap2 = subgraph.simulate_pattern_timed(&addrs, false, 4, 8, 2);

    assert!(gap2 >= no_gap);
}

#[test]
fn simulate_pattern_timed_empty() {
    let cfg = SmemFlowConfig::default();
    let subgraph = SmemSubgraph::attach(&mut FlowGraph::new(), &cfg);
    assert_eq!(subgraph.simulate_pattern_timed(&[], false, 4, 1, 0), 0);
}
