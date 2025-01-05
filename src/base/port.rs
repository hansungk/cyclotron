/// `Port` models an IO port to a component.
// TODO: having a timestamp requires this to eventually become a priority queue, to enable
// enqueueing entries N cycles in advance
use std::marker::PhantomData;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Default)]
pub struct InputPort {}

#[derive(Default)]
pub struct OutputPort {}

#[derive(Default)]
pub struct PortContent<T: Clone> {
    valid: bool,
    data: T,
}

#[derive(Default)]
pub struct Port<D, T: Clone> {
    // OnceLock ensures the port is connected to somewhere before use.
    // RwLock is necessary because each component has no knowledge of when the other component will
    // do concurrent access to the port.
    lock: OnceLock<Arc<RwLock<PortContent<T>>>>,
    // TODO: time: u64,
    direction: PhantomData<D>,
}

impl<D, T: Clone> Clone for Port<D, T> {
    fn clone(&self) -> Self {
        Self {
            lock: OnceLock::new(),
            direction: self.direction,
        }
    }
}

impl<D, T: Default + Clone> Port<D, T> {
    pub fn new() -> Port<D, T> {
        Port {
            lock: OnceLock::new(),
            direction: PhantomData,
        }
    }

    pub fn valid(&self) -> bool {
        self.lock
            .get()
            .expect("port lock not set")
            .read()
            .expect("rw lock poisoned")
            .valid
            .clone()
    }
}

impl<OutputPort, T: Default + Clone> Port<OutputPort, T> {
    pub fn blocked(&self) -> bool {
        self.valid()
    }

    // returns true if port was ready and put succeeded.
    pub fn put(&mut self, data: &T /*, time: u64*/) -> bool {
        if self.blocked() {
            return false;
        }
        // self.time = time;
        let body = &mut self
            .lock
            .get()
            .expect("lock not set")
            .write()
            .expect("lock poisoned");
        body.data = data.clone();
        body.valid = true;
        true
    }
}

impl<InputPort, T: Default + Clone> Port<InputPort, T> {
    pub fn peek(&self) -> Option<T> {
        let body = &mut self
            .lock
            .get()
            .expect("lock not set")
            .read()
            .expect("lock poisoned");
        body.valid.then_some(body.data.clone())
    }

    pub fn get(&mut self) -> Option<T> {
        self.valid().then(|| {
            let body = &mut self
                .lock
                .get()
                .expect("lock not set")
                .write()
                .expect("lock poisoned");
            body.valid = false;
            body.data.clone()
        })
    }
}

/// transfers data from an output port to an input port of the same type,
/// by giving them the same valid and data pointer
pub fn link<A, B, T: Default + Clone>(
    a: &mut Port<A, T>,
    b: &mut Port<B, T>,
) -> Arc<RwLock<PortContent<T>>> {
    let lock = Arc::new(RwLock::new(PortContent::<T> {
        valid: false,
        data: T::default(),
    }));
    a.lock
        .set(lock.clone())
        .map_err(|_| "")
        .expect("lock already set");
    b.lock
        .set(lock.clone())
        .map_err(|_| "")
        .expect("lock already set");
    lock
}

pub fn link_vec<A, B, T: Default + Clone>(
    a: &mut Vec<&mut Port<A, T>>,
    b: &mut Vec<&mut Port<B, T>>,
) -> Vec<Arc<RwLock<PortContent<T>>>> {
    a.iter_mut()
        .zip(b.iter_mut())
        .map(|(o, i)| link(o, i))
        .collect::<Vec<_>>()
}
