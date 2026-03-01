use crate::traffic::config::TrafficConfig;
use crate::traffic::patterns::PatternEngine;

#[derive(Debug, Default)]
pub struct SmemTrafficDriver {
    done: bool,
    pub pattern_engine: PatternEngine,
}

impl SmemTrafficDriver {
    pub fn new(config: &TrafficConfig) -> Self {
        Self {
            done: true,
            pattern_engine: PatternEngine::new(config),
        }
    }

    pub fn tick(&mut self) {}

    pub fn is_done(&self) -> bool {
        self.done
    }
}
