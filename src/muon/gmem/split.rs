use super::{CoreTimingModel, SmemConflictSample};
use crate::timeflow::{GmemRequest, SmemRequest};

impl CoreTimingModel {
    pub(super) fn split_gmem_request(&self, request: &GmemRequest) -> Vec<GmemRequest> {
        if !request.kind.is_mem() {
            return vec![request.clone()];
        }
        let line_bytes = self.gmem_policy.l1_line_bytes.max(1) as u64;
        let lines = request
            .coalesced_lines
            .clone()
            .unwrap_or_else(|| vec![(request.addr / line_bytes) * line_bytes]);
        if lines.is_empty() {
            return vec![request.clone()];
        }
        lines
            .into_iter()
            .map(|line| {
                let mut child = request.clone();
                child.addr = line;
                child.line_addr = line;
                child.bytes = line_bytes as u32;
                child.coalesced_lines = None;
                child.lane_addrs = None;
                child
            })
            .collect()
    }

    pub(super) fn split_smem_request(&self, request: &SmemRequest) -> Vec<SmemRequest> {
        let num_banks = self.smem_config.num_banks.max(1) as u64;
        let num_subbanks = self.smem_config.num_subbanks.max(1) as u64;
        let word_bytes = self.smem_config.word_bytes.max(1) as u64;
        let active = request.active_lanes.max(1);
        let bytes_per_lane = request.bytes.saturating_div(active).max(1);

        if let Some(lane_addrs) = request.lane_addrs.as_ref() {
            let mut groups: std::collections::HashMap<(usize, usize), (u32, u64)> =
                std::collections::HashMap::new();
            for &addr in lane_addrs {
                let word = addr / word_bytes;
                let bank = (word % num_banks) as usize;
                let subbank = ((word / num_banks) % num_subbanks) as usize;
                let entry = groups.entry((bank, subbank)).or_insert((0, addr));
                entry.0 = entry.0.saturating_add(1);
            }
            if groups.is_empty() {
                return vec![request.clone()];
            }
            return groups
                .into_iter()
                .map(|((bank, subbank), (lanes, addr))| {
                    let mut child = request.clone();
                    child.bank = bank;
                    child.subbank = subbank;
                    child.addr = addr;
                    child.active_lanes = lanes;
                    child.bytes = bytes_per_lane.saturating_mul(lanes).max(1);
                    child.lane_addrs = None;
                    child
                })
                .collect();
        }

        let word = request.addr / word_bytes;
        let bank = (word % num_banks) as usize;
        let subbank = ((word / num_banks) % num_subbanks) as usize;
        let mut child = request.clone();
        child.bank = bank;
        child.subbank = subbank;
        child.lane_addrs = None;
        vec![child]
    }

    pub(super) fn compute_smem_conflict(&self, request: &SmemRequest) -> Option<SmemConflictSample> {
        let active = request.active_lanes.max(1);
        let num_banks = self.smem_config.num_banks.max(1) as u64;
        let num_subbanks = self.smem_config.num_subbanks.max(1) as u64;
        let word_bytes = self.smem_config.word_bytes.max(1) as u64;

        if let Some(lane_addrs) = request.lane_addrs.as_ref() {
            if lane_addrs.is_empty() {
                return None;
            }
            let mut banks = std::collections::HashSet::new();
            let mut subbanks = std::collections::HashSet::new();
            for &addr in lane_addrs {
                let word = addr / word_bytes;
                let bank = (word % num_banks) as usize;
                let subbank = ((word / num_banks) % num_subbanks) as usize;
                banks.insert(bank);
                subbanks.insert((bank, subbank));
            }
            let unique_banks = banks.len() as u32;
            let unique_subbanks = subbanks.len() as u32;
            let conflict_lanes = active.saturating_sub(unique_banks.max(1));
            return Some(SmemConflictSample {
                active_lanes: active,
                unique_banks,
                unique_subbanks,
                conflict_lanes,
            });
        }

        let unique_banks = 1;
        let unique_subbanks = 1;
        let conflict_lanes = active.saturating_sub(1);
        Some(SmemConflictSample {
            active_lanes: active,
            unique_banks,
            unique_subbanks,
            conflict_lanes,
        })
    }
}
