use std::sync::Arc;

pub trait ModuleBehaviors {
    fn tick_one(&mut self);
    fn tick(&mut self, cycles: u64) {
        for _ in 0..cycles {
            self.tick_one()
        }
    }
    fn is_stalled(&self) -> bool {
        false
    }
    fn reset(&mut self) {}
}

pub trait Parameterizable {
    type ConfigType;

    fn conf(&self) -> &Self::ConfigType;
    fn init_conf(&mut self, conf: Arc<Self::ConfigType>);
}
