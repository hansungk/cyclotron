use crate::timeflow::types::CoreFlowPayload;
use crate::timeq::{Cycle, ServiceRequest, Ticket};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmemRequestKind {
    Load,
    Store,
    FlushL0,
    FlushL1,
}

impl GmemRequestKind {
    pub fn is_mem(self) -> bool {
        matches!(self, Self::Load | Self::Store)
    }

    pub fn is_flush_l0(self) -> bool {
        matches!(self, Self::FlushL0)
    }

    pub fn is_flush_l1(self) -> bool {
        matches!(self, Self::FlushL1)
    }
}

#[derive(Debug, Clone)]
pub struct GmemRequest {
    pub id: u64,
    pub core_id: usize,
    pub cluster_id: usize,
    pub warp: usize,
    pub addr: u64,
    pub line_addr: u64,
    pub lane_addrs: Option<Vec<u64>>,
    pub coalesced_lines: Option<Vec<u64>>,
    pub bytes: u32,
    pub active_lanes: u32,
    pub is_load: bool,
    pub stall_on_completion: bool,
    pub kind: GmemRequestKind,
    pub l0_hit: bool,
    pub l1_hit: bool,
    pub l2_hit: bool,
    pub l1_writeback: bool,
    pub l2_writeback: bool,
    pub l1_bank: usize,
    pub l2_bank: usize,
}

impl GmemRequest {
    pub fn new(warp: usize, bytes: u32, active_lanes: u32, is_load: bool) -> Self {
        let kind = if is_load {
            GmemRequestKind::Load
        } else {
            GmemRequestKind::Store
        };
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            lane_addrs: None,
            coalesced_lines: None,
            bytes,
            active_lanes,
            is_load,
            stall_on_completion: is_load,
            kind,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
        }
    }

    pub fn new_flush_l0(warp: usize, bytes: u32) -> Self {
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            lane_addrs: None,
            coalesced_lines: None,
            bytes,
            active_lanes: 0,
            is_load: false,
            stall_on_completion: true,
            kind: GmemRequestKind::FlushL0,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
        }
    }

    pub fn new_flush_l1(warp: usize, bytes: u32) -> Self {
        Self {
            id: 0,
            core_id: 0,
            cluster_id: 0,
            warp,
            addr: 0,
            line_addr: 0,
            lane_addrs: None,
            coalesced_lines: None,
            bytes,
            active_lanes: 0,
            is_load: false,
            stall_on_completion: true,
            kind: GmemRequestKind::FlushL1,
            l0_hit: false,
            l1_hit: false,
            l2_hit: false,
            l1_writeback: false,
            l2_writeback: false,
            l1_bank: 0,
            l2_bank: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GmemCompletion {
    pub request: GmemRequest,
    pub ticket_ready_at: Cycle,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone)]
pub struct GmemIssue {
    pub request_id: u64,
    pub ticket: Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmemRejectReason {
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct GmemReject {
    pub request: GmemRequest,
    pub retry_at: Cycle,
    pub reason: GmemRejectReason,
}

/// Convenience alias for functions that return GmemReject on error.
pub type GmemResult<T> = Result<T, GmemReject>;

pub(crate) fn extract_gmem_request(request: ServiceRequest<CoreFlowPayload>) -> GmemRequest {
    match request.payload {
        CoreFlowPayload::Gmem(req) => req,
        _ => panic!("expected gmem request"),
    }
}
