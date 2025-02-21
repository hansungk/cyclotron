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
pub struct Channel<T: Clone> {
    valid: bool,
    data: T,
}

/// Wrapper type of a reference to a channel.  Newtype is necessary to implement get/put methods at
/// the reference type.
pub struct ChannelRef<T: Clone>(Arc<RwLock<Channel<T>>>);

#[derive(Default)]
pub struct Port<D, T: Clone> {
    // RwLock is necessary because each component has no knowledge of when the other component will
    // do concurrent access to the port.
    lock: OnceLock<ChannelRef<T>>,
    // TODO: time: u64,
    direction: PhantomData<D>,
}

// FIXME: only needed for vec![v; N] initialization
impl<D, T: Clone> Clone for Port<D, T> {
    fn clone(&self) -> Self {
        Self {
            lock: OnceLock::new(),
            direction: self.direction,
        }
    }
}

impl<D, T: Default + Clone> Port<D, T> {
    pub fn new() -> Self {
        Port {
            lock: OnceLock::new(),
            direction: PhantomData,
        }
    }

    pub fn valid(&self) -> bool {
        self.lock.get().expect("port lock not set").valid()
    }
}

impl<OutputPort, T: Default + Clone> Port<OutputPort, T> {
    pub fn blocked(&self) -> bool {
        self.valid()
    }

    /// Access method of an output port from *within* the module that has the port.
    pub fn put(&mut self, data: &T /*, time: u64*/) -> bool {
        self.lock.get().expect("port lock not set").put(data)
    }
}

impl<InputPort, T: Default + Clone> Port<InputPort, T> {
    pub fn peek(&self) -> Option<T> {
        self.lock.get().expect("lock not set").peek()
    }

    /// Access method of an input port from *within* the module that has the port.
    pub fn get(&mut self) -> Option<T> {
        self.lock.get().expect("lock not set").get()
    }
}

impl<T: Clone> ChannelRef<T> {
    pub fn valid(&self) -> bool {
        self.0.read().expect("rw lock poisoned").valid
    }

    pub fn blocked(&self) -> bool {
        self.valid()
    }

    pub fn peek(&self) -> Option<T> {
        let channel = self.0.read().expect("rw lock poisoned");
        channel.valid.then_some(channel.data.clone())
    }

    /// Put a value onto the channel.
    /// Returns true if the channel was ready and the data was successfully put.
    pub fn put(&self, data: &T) -> bool {
        if self.blocked() {
            return false;
        }
        // self.time = time;
        let mut channel = self.0.write().expect("rw lock poisoned");
        channel.valid = true;
        channel.data = data.clone();
        true
    }

    /// Get a value from the channel, invalidating it.
    /// Returns Some if the channel had a valid data, or None otherwise.
    pub fn get(&self) -> Option<T> {
        let mut channel = self.0.write().expect("rw lock poisoned");
        match channel.valid {
            false => None,
            true => {
                channel.valid = false;
                Some(channel.data.clone())
            }
        }
    }
}

/// transfers data from an output port to an input port of the same type,
/// by giving them the same valid and data pointer
pub fn link<T: Default + Clone>(
    a: &mut Port<InputPort, T>,
    b: &mut Port<OutputPort, T>,
) -> ChannelRef<T> {
    let lock = Arc::new(RwLock::new(Channel::<T> {
        valid: false,
        data: T::default(),
    }));
    a.lock
        .set(ChannelRef(Arc::clone(&lock)))
        .map_err(|_| "")
        .expect("lock already set");
    b.lock
        .set(ChannelRef(Arc::clone(&lock)))
        .map_err(|_| "")
        .expect("lock already set");
    ChannelRef(lock)
}

/// Tie an output port off without connecting to another input port.
/// Used for when forwarding from an output of an inner module to the output of the wrapper module.
pub fn tie_off<T: Default + Clone>(a: &mut Port<OutputPort, T>) -> ChannelRef<T> {
    let lock = Arc::new(RwLock::new(Channel::<T> {
        valid: false,
        data: T::default(),
    }));
    a.lock
        .set(ChannelRef(Arc::clone(&lock)))
        .map_err(|_| "")
        .expect("lock already set");
    ChannelRef(lock)
}

pub fn tie_off_input<T: Default + Clone>(a: &mut Port<InputPort, T>) -> ChannelRef<T> {
    let lock = Arc::new(RwLock::new(Channel::<T> {
        valid: false,
        data: T::default(),
    }));
    a.lock
        .set(ChannelRef(Arc::clone(&lock)))
        .map_err(|_| "")
        .expect("lock already set");
    ChannelRef(lock)
}

// Lifetimes are only relevant if A, B, T are references, which should never be the case
pub fn link_iter<'a, 'b, T: Default + Clone + 'a + 'b>(
    a: impl Iterator<Item = &'a mut Port<InputPort, T>>,
    b: impl Iterator<Item = &'b mut Port<OutputPort, T>>,
) {
    Iterator::zip(a, b).for_each(|(i, o)| _ = link(i, o));
}

pub fn link_vec<T: Default + Clone>(
    a: &mut [Port<InputPort, T>],
    b: &mut [Port<OutputPort, T>],
) -> Vec<ChannelRef<T>> {
    a.iter_mut()
        .zip(b.iter_mut())
        .map(|(i, o)| link(i, o))
        .collect::<Vec<_>>()
}
