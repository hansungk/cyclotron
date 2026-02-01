use std::sync::Arc;

use crate::timeflow::graph::TimedNode;
use crate::timeq::{Backpressure, Cycle, ServiceRequest, ServiceResult, Ticket, TimedServer};

pub struct ServerNode<T> {
    name: Arc<str>,
    server: TimedServer<T>,
}

impl<T> ServerNode<T> {
    pub fn new(name: impl Into<Arc<str>>, server: TimedServer<T>) -> Self {
        Self {
            name: name.into(),
            server,
        }
    }
}

impl<T: Send + Sync + 'static> TimedNode<T> for ServerNode<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn try_put(
        &mut self,
        now: Cycle,
        request: ServiceRequest<T>,
    ) -> Result<Ticket, Backpressure<T>> {
        self.server.try_enqueue(now, request)
    }

    fn tick(&mut self, now: Cycle) {
        self.server.advance_ready(now);
    }

    fn peek_ready(&mut self, now: Cycle) -> Option<&ServiceResult<T>> {
        self.server.peek_ready(now)
    }

    fn take_ready(&mut self, now: Cycle) -> Option<ServiceResult<T>> {
        self.server.pop_ready(now)
    }

    fn outstanding(&self) -> usize {
        self.server.outstanding()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeq::{ServerConfig, ServiceRequest, TimedServer};

    fn make_node(base_latency: u64, queue_capacity: usize) -> ServerNode<u32> {
        ServerNode::new(
            "node",
            TimedServer::new(ServerConfig {
                base_latency,
                bytes_per_cycle: 4,
                queue_capacity,
                ..ServerConfig::default()
            }),
        )
    }

    #[test]
    fn server_node_name_preserved() {
        let node = ServerNode::new("test_node", TimedServer::<u32>::new(ServerConfig::default()));
        assert_eq!("test_node", node.name());
    }

    #[test]
    fn server_node_tick_advances_server() {
        let mut node = make_node(1, 2);
        let ticket = node.try_put(0, ServiceRequest::new(42u32, 4)).unwrap();
        assert!(node.peek_ready(ticket.ready_at().saturating_sub(1)).is_none());
        node.tick(ticket.ready_at());
        assert!(node.peek_ready(ticket.ready_at()).is_some());
    }

    #[test]
    fn server_node_take_ready_returns_completion() {
        let mut node = make_node(0, 2);
        let ticket = node.try_put(0, ServiceRequest::new(7u32, 4)).unwrap();
        node.tick(ticket.ready_at());
        let result = node
            .take_ready(ticket.ready_at())
            .expect("completion should exist");
        assert_eq!(7, result.payload);
    }

    #[test]
    fn server_node_outstanding_count_accurate() {
        let mut node = make_node(1, 4);
        node.try_put(0, ServiceRequest::new(1u32, 4)).unwrap();
        node.try_put(0, ServiceRequest::new(2u32, 4)).unwrap();
        node.try_put(0, ServiceRequest::new(3u32, 4)).unwrap();
        assert_eq!(3, node.outstanding());
    }

    #[test]
    fn server_node_peek_ready_non_consuming() {
        let mut node = make_node(0, 2);
        let ticket = node.try_put(0, ServiceRequest::new(9u32, 4)).unwrap();
        node.tick(ticket.ready_at());
        assert!(node.peek_ready(ticket.ready_at()).is_some());
        assert!(node.peek_ready(ticket.ready_at()).is_some());
        assert!(node.take_ready(ticket.ready_at()).is_some());
    }

    #[test]
    fn server_node_empty_peek_returns_none() {
        let mut node = ServerNode::new(
            "node",
            TimedServer::<u32>::new(ServerConfig::default()),
        );
        assert!(node.peek_ready(0).is_none());
    }

    #[test]
    fn server_node_empty_take_returns_none() {
        let mut node = ServerNode::new(
            "node",
            TimedServer::<u32>::new(ServerConfig::default()),
        );
        assert!(node.take_ready(0).is_none());
    }
}
