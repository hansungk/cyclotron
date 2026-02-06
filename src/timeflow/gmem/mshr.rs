use crate::timeq::Cycle;

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
    pub(crate) merged: Vec<GmemRequest>,
}

impl MshrEntry {
    fn new(line_addr: u64, meta: MissMetadata) -> Self {
        Self {
            line_addr,
            meta,
            ready_at: None,
            merged: Vec::new(),
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
