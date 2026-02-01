use crate::timeq::{Backpressure, Cycle, ServerConfig, ServiceRequest, TimedServer};
use smallvec::SmallVec;
use std::cmp::max;

use super::request::GmemRequest;

#[derive(Debug, Clone, Copy)]
pub(crate) struct MissMetadata {
    line_addr: u64,
    l0_hit: bool,
    l1_hit: bool,
    l2_hit: bool,
    l1_writeback: bool,
    l2_writeback: bool,
    l1_bank: usize,
    l2_bank: usize,
}

impl MissMetadata {
    pub(crate) fn from_request(request: &GmemRequest) -> Self {
        Self {
            line_addr: request.line_addr,
            l0_hit: request.l0_hit,
            l1_hit: request.l1_hit,
            l2_hit: request.l2_hit,
            l1_writeback: request.l1_writeback,
            l2_writeback: request.l2_writeback,
            l1_bank: request.l1_bank,
            l2_bank: request.l2_bank,
        }
    }

    pub(crate) fn apply(&self, request: &mut GmemRequest) {
        request.line_addr = self.line_addr;
        request.l0_hit = self.l0_hit;
        request.l1_hit = self.l1_hit;
        request.l2_hit = self.l2_hit;
        request.l1_writeback = self.l1_writeback;
        request.l2_writeback = self.l2_writeback;
        request.l1_bank = self.l1_bank;
        request.l2_bank = self.l2_bank;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissLevel {
    None,
    L0,
    L1,
    L2,
}

#[derive(Debug)]
pub(crate) struct MshrEntry {
    line_addr: u64,
    meta: MissMetadata,
    ready_at: Option<Cycle>,
    pub(crate) merged: SmallVec<[GmemRequest; 2]>,
}

impl MshrEntry {
    fn new(line_addr: u64, meta: MissMetadata) -> Self {
        Self {
            line_addr,
            meta,
            ready_at: None,
            merged: SmallVec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct MshrTable {
    capacity: usize,
    entries: Vec<MshrEntry>,
}

impl MshrTable {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::new(),
        }
    }

    pub(crate) fn has_entry(&self, line_addr: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.line_addr == line_addr)
    }

    pub(crate) fn can_allocate(&self, line_addr: u64) -> bool {
        self.has_entry(line_addr) || self.entries.len() < self.capacity
    }

    pub(crate) fn ensure_entry(&mut self, line_addr: u64, meta: MissMetadata) -> Result<bool, ()> {
        if self.has_entry(line_addr) {
            return Ok(false);
        }
        if self.entries.len() >= self.capacity {
            return Err(());
        }
        self.entries.push(MshrEntry::new(line_addr, meta));
        Ok(true)
    }

    pub(crate) fn set_ready_at(&mut self, line_addr: u64, ready_at: Cycle) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.line_addr == line_addr)
        {
            entry.ready_at = Some(ready_at);
        }
    }

    pub(crate) fn merge_request(
        &mut self,
        line_addr: u64,
        mut request: GmemRequest,
    ) -> Option<Cycle> {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.line_addr == line_addr)
        {
            entry.meta.apply(&mut request);
            entry.merged.push(request);
            return entry.ready_at;
        }
        None
    }

    pub(crate) fn remove_entry(&mut self, line_addr: u64) -> Option<MshrEntry> {
        let idx = self
            .entries
            .iter()
            .position(|entry| entry.line_addr == line_addr)?;
        Some(self.entries.swap_remove(idx))
    }
}

pub(crate) struct MshrAdmission {
    server: TimedServer<()>,
}

pub(crate) enum AdmissionBackpressure {
    Busy { retry_at: Cycle },
    QueueFull { retry_at: Cycle },
}

impl MshrAdmission {
    pub(crate) fn new(config: ServerConfig) -> Self {
        let mut cfg = config;
        cfg.base_latency = 0;
        cfg.completions_per_cycle = u32::MAX;
        Self {
            server: TimedServer::new(cfg),
        }
    }

    pub(crate) fn try_admit(&mut self, now: Cycle) -> Result<Cycle, AdmissionBackpressure> {
        let request = ServiceRequest::new((), 0);
        match self.server.try_enqueue(now, request) {
            Ok(ticket) => Ok(ticket.ready_at()),
            Err(Backpressure::Busy { available_at, .. }) => Err(AdmissionBackpressure::Busy {
                retry_at: max(available_at, now.saturating_add(1)),
            }),
            Err(Backpressure::QueueFull { .. }) => {
                let retry_at = self
                    .server
                    .oldest_ticket()
                    .map(|ticket| ticket.ready_at())
                    .unwrap_or_else(|| self.server.available_at());
                Err(AdmissionBackpressure::QueueFull {
                    retry_at: max(retry_at, now.saturating_add(1)),
                })
            }
        }
    }

    pub(crate) fn tick(&mut self, now: Cycle) {
        self.server.service_ready(now, |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::{MissMetadata, MshrTable};
    use crate::timeflow::GmemRequest;

    #[test]
    fn mshr_table_merges_requests() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.ensure_entry(1, meta).unwrap());
        assert!(table.ensure_entry(2, meta).is_err());
        table.set_ready_at(1, 10);
        let merged = GmemRequest::new(1, 8, 0xF, true);
        let ready_at = table.merge_request(1, merged).unwrap();
        assert_eq!(ready_at, 10);
        let entry = table.remove_entry(1).unwrap();
        assert_eq!(entry.merged.len(), 1);
    }

    #[test]
    fn new_mshr_table_is_empty() {
        let table = MshrTable::new(4);
        assert!(!table.has_entry(0));
        assert!(!table.has_entry(123));
    }

    #[test]
    fn ensure_entry_existing_returns_false() {
        let mut table = MshrTable::new(2);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.ensure_entry(7, meta).unwrap());
        assert!(!table.ensure_entry(7, meta).unwrap());
    }

    #[test]
    fn merge_request_to_nonexistent_returns_none() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 8, 0xF, true);
        assert!(table.merge_request(1, req).is_none());
    }

    #[test]
    fn remove_entry_frees_slot() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.ensure_entry(1, meta).unwrap());
        assert!(!table.can_allocate(2));
        table.remove_entry(1).unwrap();
        assert!(table.can_allocate(2));
    }

    #[test]
    fn can_allocate_true_when_under_capacity() {
        let mut table = MshrTable::new(2);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.can_allocate(1));
        assert!(table.ensure_entry(1, meta).unwrap());
        assert!(table.can_allocate(2));
    }

    #[test]
    fn can_allocate_false_when_full_different_line() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.ensure_entry(1, meta).unwrap());
        assert!(!table.can_allocate(2));
    }

    #[test]
    fn set_ready_at_updates_entry() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        table.ensure_entry(1, meta).unwrap();
        table.set_ready_at(1, 42);
        let merged = GmemRequest::new(0, 8, 0xF, true);
        let ready_at = table.merge_request(1, merged).unwrap();
        assert_eq!(42, ready_at);
    }

    #[test]
    fn capacity_of_one() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        assert!(table.ensure_entry(1, meta).unwrap());
        assert!(!table.can_allocate(2));
    }

    #[test]
    fn multiple_merges_same_entry() {
        let mut table = MshrTable::new(1);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        table.ensure_entry(1, meta).unwrap();
        for _ in 0..10 {
            let merged = GmemRequest::new(0, 8, 0xF, true);
            assert!(table.merge_request(1, merged).is_none());
        }
        let entry = table.remove_entry(1).unwrap();
        assert_eq!(entry.merged.len(), 10);
    }

    #[test]
    fn fill_and_drain_repeatedly() {
        let mut table = MshrTable::new(4);
        let req = GmemRequest::new(0, 16, 0xF, true);
        let meta = MissMetadata::from_request(&req);
        for round in 0..100 {
            for line in 0..4 {
                assert!(table.ensure_entry(line, meta).unwrap(), "round {round}");
            }
            for line in 0..4 {
                assert!(table.remove_entry(line).is_some(), "round {round}");
            }
        }
    }
}
