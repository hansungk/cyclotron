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

#[cfg(test)]
mod tests {
    use super::{WarpIssueScheduler, WarpSchedulerConfig};

    fn enabled_scheduler(issue_width: usize) -> WarpIssueScheduler {
        let mut cfg = WarpSchedulerConfig::default();
        cfg.enabled = true;
        cfg.issue_width = issue_width;
        WarpIssueScheduler::new(cfg)
    }

    #[test]
    fn grants_round_robin_across_eligible() {
        let mut sched = enabled_scheduler(1);

        let eligible = vec![true, true, false];
        let grants0 = sched.select(0, &eligible);
        assert_eq!(grants0, vec![true, false, false]);
        let grants1 = sched.select(1, &eligible);
        assert_eq!(grants1, vec![false, true, false]);
        let grants2 = sched.select(2, &eligible);
        assert_eq!(grants2, vec![true, false, false]);
    }

    #[test]
    fn skips_ineligible_warps() {
        let mut sched = enabled_scheduler(1);

        let eligible = vec![false, true, true];
        let grants0 = sched.select(0, &eligible);
        assert_eq!(grants0, vec![false, true, false]);
        let grants1 = sched.select(1, &eligible);
        assert_eq!(grants1, vec![false, false, true]);
    }

    #[test]
    fn disabled_scheduler_passes_through() {
        let mut cfg = WarpSchedulerConfig::default();
        cfg.enabled = false;
        let mut sched = WarpIssueScheduler::new(cfg);

        let eligible = vec![true, false, true];
        let grants = sched.select(0, &eligible);
        assert_eq!(grants, eligible);
    }

    #[test]
    fn issue_width_limits_grants() {
        let mut sched = enabled_scheduler(2);

        let eligible = vec![true, true, true, false];
        let grants = sched.select(0, &eligible);
        assert_eq!(grants.iter().filter(|&&g| g).count(), 2);
    }

    #[test]
    fn no_eligible_returns_all_false() {
        let mut sched = enabled_scheduler(2);

        let eligible = vec![false, false, false];
        let grants = sched.select(0, &eligible);
        assert_eq!(grants, vec![false, false, false]);
    }

    #[test]
    fn scheduler_handles_single_warp() {
        let mut sched = enabled_scheduler(1);

        let eligible = vec![true];
        let grants = sched.select(0, &eligible);
        assert_eq!(grants, vec![true]);
    }

    #[test]
    fn scheduler_handles_all_warps_stalled() {
        let mut sched = enabled_scheduler(2);

        let eligible = vec![false, false, false, false];
        let grants = sched.select(0, &eligible);
        assert_eq!(grants, vec![false, false, false, false]);
    }

    #[test]
    fn scheduler_round_robin_wraps_around() {
        let mut sched = enabled_scheduler(1);

        let eligible = vec![true, true, true];
        let g0 = sched.select(0, &eligible);
        let g1 = sched.select(1, &eligible);
        let g2 = sched.select(2, &eligible);
        let g3 = sched.select(3, &eligible);
        assert_eq!(g0, vec![true, false, false]);
        assert_eq!(g1, vec![false, true, false]);
        assert_eq!(g2, vec![false, false, true]);
        assert_eq!(g3, vec![true, false, false]);
    }
}
