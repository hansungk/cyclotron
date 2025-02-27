use std::sync::Arc;
use crate::base::behavior::*;
use crate::base::component::{component_inner, ComponentBase, IsComponent};

pub struct StackState<T, const N: usize> {
    pub storage: Vec<T>,
    size: usize,
    max_size: usize,
}

impl<T: Default, const N: usize> Default for StackState<T, N> {
    fn default() -> Self {
        Self {
            storage: (0..N).map(|_| T::default()).collect(), // no need for Copy trait
            size: 0,
            max_size: N,
        }
    }
}

#[derive(Default)]
pub struct Stack<T, const N: usize> where T: Default {
    base: ComponentBase<StackState<T, N>, ()>,
}

impl<T: Default, const N: usize> Stack<T, N> {
    pub fn new(_: Arc<()>) -> Self {
        Stack::<T, N>::default()
    }
}

impl<T: Default, const N: usize> ComponentBehaviors for Stack<T, N> {
    fn tick_one(&mut self) {}
    fn reset(&mut self) {
        self.state().size = 0;
    }
}

impl<T: Default, const N: usize> IsComponent for Stack<T, N> {
    component_inner!(StackState<T, N>, ());

}

// TODO: add locks and stuff
impl<T: Default + Clone, const N: usize> Stack<T, N> {
    pub fn try_push(&mut self, data: &T) -> bool {
        let size = self.state().size;
        let max_size = self.state().max_size;
        if size >= max_size {
            return false;
        }
        self.state().storage[size] = data.clone();
        self.state().size += 1;
        true
    }

    pub fn try_pop(&mut self) -> Option<T> where T: Clone {
        let size = self.state().size;
        (size > 0).then(|| {
            self.state().size -= 1;
            self.state().storage[size - 1].clone()
        })
    }

    pub fn resize(&mut self, size: usize) {
        self.state().max_size = size;
    }
}
