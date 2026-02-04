use crate::timeflow::graph::{FlowGraph, Link, TimedNode};
use crate::timeflow::server_node::ServerNode;
use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, ServiceResult, TimedServer, Ticket};

#[test]
fn graph_moves_payloads_between_nodes() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let node0 = ServerNode::new(
        "n0",
        TimedServer::new(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    );
    let node1 = ServerNode::new(
        "n1",
        TimedServer::new(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    );
    let n0 = graph.add_node(node0);
    let n1 = graph.add_node(node1);
    graph.connect(n0, n1, "n0->n1", Link::new(4));

    graph
        .try_put(n0, 0, ServiceRequest::new("payload", 8))
        .expect("enqueue should succeed");

    for cycle in 0..10 {
        graph.tick(cycle);
    }

    graph.with_node_mut(n1, |node| {
        let result = node.take_ready(10).expect("result should exist");
        assert_eq!("payload", result.payload);
    });
}

#[test]
fn filtered_links_route_payloads() {
    #[derive(Clone, Debug)]
    struct Payload {
        bank: usize,
        value: &'static str,
    }
    let mut graph: FlowGraph<Payload> = FlowGraph::new();
    let src = graph.add_node(ServerNode::new(
        "src",
        TimedServer::new(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        }),
    ));
    let sink0 = graph.add_node(ServerNode::new(
        "sink0",
        TimedServer::new(ServerConfig::default()),
    ));
    let sink1 = graph.add_node(ServerNode::new(
        "sink1",
        TimedServer::new(ServerConfig::default()),
    ));

    graph.connect_filtered(src, sink0, "src->0", Link::new(4), |payload: &Payload| {
        payload.bank == 0
    });
    graph.connect_filtered(src, sink1, "src->1", Link::new(4), |payload: &Payload| {
        payload.bank == 1
    });

    graph
        .try_put(
            src,
            0,
            ServiceRequest::new(
                Payload {
                    bank: 0,
                    value: "A",
                },
                4,
            ),
        )
        .unwrap();
    graph
        .try_put(
            src,
            1,
            ServiceRequest::new(
                Payload {
                    bank: 1,
                    value: "B",
                },
                4,
            ),
        )
        .unwrap();

    for cycle in 0..10 {
        graph.tick(cycle);
    }

    graph.with_node_mut(sink0, |node| {
        let result = node.take_ready(10).expect("sink0 result");
        assert_eq!("A", result.payload.value);
    });
    graph.with_node_mut(sink1, |node| {
        let result = node.take_ready(10).expect("sink1 result");
        assert_eq!("B", result.payload.value);
    });
}

#[test]
fn route_fn_selects_output_by_index() {
    #[derive(Clone, Debug)]
    struct Payload {
        route: usize,
        value: &'static str,
    }

    let mut graph: FlowGraph<Payload> = FlowGraph::new();
    let src = graph.add_node(ServerNode::new(
        "src",
        TimedServer::new(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 8,
            ..ServerConfig::default()
        }),
    ));
    let sink0 = graph.add_node(ServerNode::new(
        "sink0",
        TimedServer::new(ServerConfig::default()),
    ));
    let sink1 = graph.add_node(ServerNode::new(
        "sink1",
        TimedServer::new(ServerConfig::default()),
    ));

    graph.connect(src, sink0, "src->0", Link::new(4));
    graph.connect(src, sink1, "src->1", Link::new(4));
    graph.set_route_fn(src, |payload: &Payload| payload.route);

    graph
        .try_put(
            src,
            0,
            ServiceRequest::new(
                Payload {
                    route: 0,
                    value: "A",
                },
                4,
            ),
        )
        .unwrap();
    graph
        .try_put(
            src,
            1,
            ServiceRequest::new(
                Payload {
                    route: 1,
                    value: "B",
                },
                4,
            ),
        )
        .unwrap();

    for cycle in 0..10 {
        graph.tick(cycle);
    }

    graph.with_node_mut(sink0, |node| {
        let result = node.take_ready(10).expect("sink0 result");
        assert_eq!("A", result.payload.value);
    });
    graph.with_node_mut(sink1, |node| {
        let result = node.take_ready(10).expect("sink1 result");
        assert_eq!("B", result.payload.value);
    });
}

#[test]
fn single_node_graph_processes_requests() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let node = graph.add_node(ServerNode::new(
        "node",
        TimedServer::new(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));

    graph
        .try_put(node, 0, ServiceRequest::new("payload", 4))
        .expect("enqueue should succeed");
    for cycle in 0..3 {
        graph.tick(cycle);
    }
    graph.with_node_mut(node, |n| {
        let result = n.take_ready(2).expect("completion exists");
        assert_eq!("payload", result.payload);
    });
}

#[test]
fn two_node_linear_chain_latency_accumulates() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let a = graph.add_node(ServerNode::new(
        "a",
        TimedServer::new(ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));
    let b = graph.add_node(ServerNode::new(
        "b",
        TimedServer::new(ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));
    graph.connect(a, b, "a->b", Link::new(4));
    graph
        .try_put(a, 0, ServiceRequest::new("payload", 4))
        .expect("enqueue should succeed");

    for cycle in 0..10 {
        graph.tick(cycle);
    }
    graph.with_node_mut(b, |n| {
        let result = n.take_ready(10).expect("completion exists");
        assert_eq!("payload", result.payload);
    });
}

#[test]
fn empty_graph_tick_is_noop() {
    let mut graph: FlowGraph<u32> = FlowGraph::new();
    graph.tick(0);
    graph.tick(1);
}

#[test]
fn node_with_no_outputs_accumulates_completions() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let node = graph.add_node(ServerNode::new(
        "leaf",
        TimedServer::new(ServerConfig {
            base_latency: 0,
            bytes_per_cycle: 4,
            queue_capacity: 2,
            ..ServerConfig::default()
        }),
    ));
    graph
        .try_put(node, 0, ServiceRequest::new("payload", 4))
        .expect("enqueue should succeed");
    graph.tick(1);
    graph.with_node_mut(node, |n| {
        assert!(n.peek_ready(1).is_some());
    });
}

#[test]
fn filtered_link_with_false_predicate_blocks() {
    let mut graph: FlowGraph<u32> = FlowGraph::new();
    let src = graph.add_node(ServerNode::new(
        "src",
        TimedServer::new(ServerConfig {
            queue_capacity: 32,
            ..ServerConfig::default()
        }),
    ));
    let dst = graph.add_node(ServerNode::new(
        "dst",
        TimedServer::new(ServerConfig::default()),
    ));
    graph.connect_filtered(src, dst, "src->dst", Link::new(2), |_| false);
    graph.try_put(src, 0, ServiceRequest::new(7u32, 4)).unwrap();
    for cycle in 0..5 {
        graph.tick(cycle);
    }
    graph.with_node_mut(dst, |n| {
        assert!(n.peek_ready(10).is_none());
    });
}

#[test]
fn three_node_linear_chain() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let a = graph.add_node(ServerNode::new(
        "a",
        TimedServer::new(ServerConfig {
            base_latency: 1,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));
    let b = graph.add_node(ServerNode::new(
        "b",
        TimedServer::new(ServerConfig {
            base_latency: 2,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));
    let c = graph.add_node(ServerNode::new(
        "c",
        TimedServer::new(ServerConfig {
            base_latency: 3,
            bytes_per_cycle: 4,
            queue_capacity: 4,
            ..ServerConfig::default()
        }),
    ));
    graph.connect(a, b, "a->b", Link::new(4));
    graph.connect(b, c, "b->c", Link::new(4));

    graph
        .try_put(a, 0, ServiceRequest::new("payload", 4))
        .expect("enqueue should succeed");
    for cycle in 0..12 {
        graph.tick(cycle);
    }
    graph.with_node_mut(c, |n| {
        let result = n.take_ready(12).expect("completion exists");
        assert_eq!("payload", result.payload);
    });
}

#[test]
fn multiple_inputs_to_one_node() {
    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let src0 = graph.add_node(ServerNode::new(
        "src0",
        TimedServer::new(ServerConfig::default()),
    ));
    let src1 = graph.add_node(ServerNode::new(
        "src1",
        TimedServer::new(ServerConfig::default()),
    ));
    let sink = graph.add_node(ServerNode::new(
        "sink",
        TimedServer::new(ServerConfig {
            queue_capacity: 2,
            ..ServerConfig::default()
        }),
    ));
    graph.connect(src0, sink, "src0->sink", Link::new(4));
    graph.connect(src1, sink, "src1->sink", Link::new(4));

    graph.try_put(src0, 0, ServiceRequest::new("a", 1)).unwrap();
    graph.try_put(src1, 0, ServiceRequest::new("b", 1)).unwrap();
    for cycle in 0..5 {
        graph.tick(cycle);
    }
    graph.with_node_mut(sink, |n| {
        let mut results = Vec::new();
        while let Some(res) = n.take_ready(5) {
            results.push(res.payload);
        }
        results.sort();
        assert_eq!(results, vec!["a", "b"]);
    });
}

#[test]
fn deep_pipeline_100_nodes() {
    let mut graph: FlowGraph<u32> = FlowGraph::new();
    let mut nodes = Vec::new();
    for i in 0..100 {
        let node = graph.add_node(ServerNode::new(
            format!("n{i}"),
            TimedServer::new(ServerConfig {
                base_latency: 0,
                bytes_per_cycle: 1,
                queue_capacity: 2,
                ..ServerConfig::default()
            }),
        ));
        nodes.push(node);
    }
    for i in 0..99 {
        graph.connect(
            nodes[i],
            nodes[i + 1],
            format!("n{i}->n{}", i + 1),
            Link::new(2),
        );
    }

    graph
        .try_put(nodes[0], 0, ServiceRequest::new(42u32, 1))
        .unwrap();
    for cycle in 0..200 {
        graph.tick(cycle);
    }
    graph.with_node_mut(nodes[99], |n| {
        let result = n.take_ready(200).expect("completion exists");
        assert_eq!(42, result.payload);
    });
}

#[test]
fn wide_fanout_32_branches() {
    #[derive(Clone, Debug)]
    struct Payload {
        branch: usize,
    }
    let mut graph: FlowGraph<Payload> = FlowGraph::new();
    let src = graph.add_node(ServerNode::new(
        "src",
        TimedServer::new(ServerConfig {
            queue_capacity: 32,
            ..ServerConfig::default()
        }),
    ));
    let mut sinks = Vec::new();
    for i in 0..32 {
        let sink = graph.add_node(ServerNode::new(
            format!("sink{i}"),
            TimedServer::new(ServerConfig::default()),
        ));
        graph.connect_filtered(
            src,
            sink,
            format!("src->sink{i}"),
            Link::new(4),
            move |payload: &Payload| payload.branch == i,
        );
        sinks.push(sink);
    }

    for i in 0..32 {
        graph
            .try_put(src, 0, ServiceRequest::new(Payload { branch: i }, 1))
            .unwrap();
    }

    for cycle in 0..50 {
        graph.tick(cycle);
    }

    for (i, &sink) in sinks.iter().enumerate() {
        graph.with_node_mut(sink, |n| {
            let result = n.take_ready(50).expect("completion exists");
            assert_eq!(result.payload.branch, i);
        });
    }
}

#[test]
fn busy_nodes_propagate_backpressure() {
    struct BusyNode;
    impl TimedNode<&'static str> for BusyNode {
        fn name(&self) -> &str {
            "busy"
        }
        fn try_put(
            &mut self,
            now: Cycle,
            request: ServiceRequest<&'static str>,
        ) -> Result<Ticket, Backpressure<&'static str>> {
            Err(Backpressure::Busy {
                request,
                available_at: now + 5,
            })
        }
        fn tick(&mut self, _now: Cycle) {}
        fn peek_ready(&mut self, _now: Cycle) -> Option<&ServiceResult<&'static str>> {
            None
        }
        fn take_ready(&mut self, _now: Cycle) -> Option<ServiceResult<&'static str>> {
            None
        }
        fn outstanding(&self) -> usize {
            0
        }
    }

    let mut graph: FlowGraph<&'static str> = FlowGraph::new();
    let src = graph.add_node(ServerNode::new(
        "src",
        TimedServer::new(ServerConfig::default()),
    ));
    let dst = graph.add_node(BusyNode);
    graph.connect(src, dst, "link", Link::new(2));
    graph
        .try_put(src, 0, ServiceRequest::new("req", 4))
        .unwrap();

    for cycle in 0..5 {
        graph.tick(cycle);
    }

    let stats = graph.edge_stats(0);
    assert!(stats.downstream_backpressure > 0);
    assert!(stats.last_delivery_cycle.is_none());
}
