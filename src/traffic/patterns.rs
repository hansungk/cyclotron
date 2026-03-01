use crate::traffic::config::{TrafficConfig, TrafficPatternSpec};

#[derive(Debug, Clone, Default)]
pub struct PatternEngine {
    pub patterns: Vec<TrafficPatternSpec>,
}

impl PatternEngine {
    pub fn new(config: &TrafficConfig) -> Self {
        Self {
            patterns: config.patterns.clone(),
        }
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }
}

