use serde::Deserialize;

use crate::timeq::Cycle;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WarpSchedulerConfig {
    pub enabled: bool,
    pub issue_width: usize,
}

impl Default for WarpSchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issue_width: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WarpIssueScheduler {
    enabled: bool,
    issue_width: usize,
    rr_cursor: usize,
    last_cycle: Option<Cycle>,
}

impl WarpIssueScheduler {
    pub fn new(config: WarpSchedulerConfig) -> Self {
        let issue_width = config.issue_width.max(1);
        Self {
            enabled: config.enabled,
            issue_width,
            rr_cursor: 0,
            last_cycle: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn select(&mut self, now: Cycle, eligible: &[bool]) -> Vec<bool> {
        if !self.enabled {
            return eligible.to_vec();
        }
        let n = eligible.len();
        if n == 0 {
            return Vec::new();
        }
        if self.last_cycle != Some(now) {
            self.last_cycle = Some(now);
        }

        let mut grants = vec![false; n];
        let mut granted = 0usize;
        let start = self.rr_cursor % n;
        for offset in 0..n {
            let wid = (start + offset) % n;
            if eligible[wid] {
                grants[wid] = true;
                granted += 1;
                if granted >= self.issue_width {
                    self.rr_cursor = (wid + 1) % n;
                    break;
                }
            }
        }

        if granted > 0 && granted < self.issue_width {
            if let Some(last) = grants.iter().rposition(|&g| g) {
                self.rr_cursor = (last + 1) % n;
            }
        }

        grants
    }
}

