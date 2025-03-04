use std::collections::VecDeque;
use std::sync::Arc;
use crate::base::behavior::*;
use crate::base::module::{module_inner, ModuleBase, IsModule};

pub struct QueueState<T, const N: usize> {
    pub storage: VecDeque<T>,
    max_size: usize,
}

impl<T: Default, const N: usize> Default for QueueState<T, N> {
    fn default() -> Self {
        Self {
            storage: VecDeque::new(),
            max_size: N,
        }
    }
}

#[derive(Default)]
pub struct Queue<T, const N: usize> where T: Default {
    base: ModuleBase<QueueState<T, N>, ()>,
}

impl<T: Default, const N: usize> ModuleBehaviors for Queue<T, N> {
    fn tick_one(&mut self) {}
    fn reset(&mut self) {
        self.state().storage.clear();
    }
}

impl<T: Default, const N: usize> IsModule for Queue<T, N> {
    module_inner!(QueueState<T, N>, ());
}

// TODO: add locks and stuff
impl<T: Default + Clone, const N: usize> Queue<T, N> {
    pub fn new(_: Arc<()>) -> Self {
        Queue::<T, N>::default()
    }

    pub fn try_enq(&mut self, data: &T) -> bool {
        let size = self.state().storage.len();
        let max_size = self.state().max_size;
        if size >= max_size {
            return false;
        }
        self.state().storage.push_back(data.clone());
        true
    }

    pub fn try_deq(&mut self) -> Option<T> where T: Clone {
        self.state().storage.pop_front()
    }

    pub fn resize(&mut self, size: usize) {
        self.state().max_size = size;
    }
}
