use crate::timeflow::graph::TimedNode;
use crate::timeq::{Backpressure, Cycle, ServiceRequest, ServiceResult, Ticket, TimedServer};

pub struct ServerNode<T> {
    name: String,
    server: TimedServer<T>,
}

impl<T> ServerNode<T> {
    pub fn new(name: impl Into<String>, server: TimedServer<T>) -> Self {
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
