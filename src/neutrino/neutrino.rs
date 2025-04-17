use std::sync::Arc;
use crate::base::behavior::{ModuleBehaviors, Parameterizable};
use crate::base::module::IsModule;
use crate::base::module::{module, ModuleBase};
use crate::neutrino::config::NeutrinoConfig;
use crate::neutrino::scoreboard::Scoreboard;

#[derive(Default)]
pub struct NeutrinoState {
}

pub struct Neutrino {
    base: ModuleBase<NeutrinoState, NeutrinoConfig>,
    scoreboard: Scoreboard,
}

module!(Neutrino, NeutrinoState, NeutrinoConfig,);

impl ModuleBehaviors for Neutrino {
    fn tick_one(&mut self) {
        self.scoreboard.tick_one();
    }
}

impl Neutrino {
    pub fn new(config: Arc<NeutrinoConfig>) -> Self {
        let mut me = Neutrino {
            base: ModuleBase::<NeutrinoState, NeutrinoConfig> {
                state: NeutrinoState {},
                ..ModuleBase::default()
            },
            scoreboard: Scoreboard::new(config.clone()),
        };
        me.init_conf(config.clone());
        me
    }
}