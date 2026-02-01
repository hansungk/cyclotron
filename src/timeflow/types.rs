use crate::timeflow::{gmem::GmemRequest, smem::SmemRequest};

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
