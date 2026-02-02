use crate::timeflow::{gmem::GmemRequest, smem::SmemRequest};
use crate::timeq::Cycle;

pub type NodeId = usize;
pub type LinkId = usize;

#[derive(Debug, Clone)]
pub enum CoreFlowPayload {
    Gmem(GmemRequest),
    Smem(SmemRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    QueueFull,
    Busy,
}

/// Central reject type used across timeflow modules.
#[derive(Debug, Clone)]
pub struct Reject {
    pub retry_at: Cycle,
    pub reason: RejectReason,
}

/// Central reject-with-payload helper.
#[derive(Debug, Clone)]
pub struct RejectWith<T> {
    pub retry_at: Cycle,
    pub reason: RejectReason,
    pub payload: T,
}

impl<T> RejectWith<T> {
    pub fn new(payload: T, retry_at: Cycle, reason: RejectReason) -> Self {
        Self { payload, retry_at, reason }
    }
}

impl Reject {
    pub fn new(retry_at: Cycle, reason: RejectReason) -> Self {
        Self { retry_at, reason }
    }
}
