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

#[derive(Debug, Clone)]
pub struct Reject {
    pub retry_at: Cycle,
    pub reason: RejectReason,
}

#[derive(Debug, Clone)]
pub struct RejectWith<T> {
    pub retry_at: Cycle,
    pub reason: RejectReason,
    pub payload: T,
}

impl<T> RejectWith<T> {
    pub fn new(payload: T, retry_at: Cycle, reason: RejectReason) -> Self {
        Self {
            payload,
            retry_at,
            reason,
        }
    }
}

impl Reject {
    pub fn new(retry_at: Cycle, reason: RejectReason) -> Self {
        Self { retry_at, reason }
    }
}

pub trait HasMemoryMetadata {
    fn id(&self) -> u64;
    fn bytes(&self) -> u32;
}

impl HasMemoryMetadata for GmemRequest {
    fn id(&self) -> u64 {
        self.id
    }
    fn bytes(&self) -> u32 {
        self.bytes
    }
}

impl HasMemoryMetadata for SmemRequest {
    fn id(&self) -> u64 {
        self.id
    }
    fn bytes(&self) -> u32 {
        self.bytes
    }
}
