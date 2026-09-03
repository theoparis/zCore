use alloc::{boxed::Box, vec::Vec};

use crate::sync::Mutex;

/// A type alias for the closure to handle device event.
pub type EventHandler<T = ()> = Box<dyn Fn(&T) + Send + Sync>;

const MAX_ONCE_HANDLERS: usize = 64;

/// Device event listener.
///
/// It keeps a series of [`EventHandler`]s that handle events of one single type.
pub struct EventListener<T = ()> {
    events: Mutex<Vec<(u64, EventHandler<T>, bool)>>,
    next_id: Mutex<u64>,
}

impl<T> EventListener<T> {
    /// Create a new event listener.
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
        }
    }

    /// Register a new `handler` into this `EventListener`.
    ///
    /// If `once` is `true`, the `handler` will be removed once it handles an event.
    /// Returns a subscription id for [`Self::unsubscribe`].
    pub fn subscribe(&self, handler: EventHandler<T>, once: bool) -> Option<u64> {
        let mut events = self.events.lock();
        if once {
            let once_count = events.iter().filter(|(_, _, o)| *o).count();
            if once_count >= MAX_ONCE_HANDLERS {
                if let Some(pos) = events.iter().position(|(_, _, o)| *o) {
                    drop(events.remove(pos));
                }
            }
        }
        let mut next = self.next_id.lock();
        let id = *next;
        *next = next.wrapping_add(1);
        events.push((id, handler, once));
        Some(id)
    }

    /// Remove a previously registered handler by id (no-op if already fired).
    pub fn unsubscribe(&self, id: u64) {
        self.events.lock().retain(|(item_id, _, _)| *item_id != id);
    }

    /// Send an event to the `EventListener`.
    pub fn trigger(&self, event: T) {
        let drained: Vec<(u64, EventHandler<T>, bool)> = {
            let mut guard = self.events.lock();
            guard.drain(..).collect()
        };
        let mut kept = Vec::with_capacity(drained.len());
        for (id, f, once) in drained {
            f(&event);
            if !once {
                kept.push((id, f, once));
            }
        }
        if kept.is_empty() {
            return;
        }
        let mut guard = self.events.lock();
        kept.append(&mut *guard);
        *guard = kept;
    }
}

impl<T> Default for EventListener<T> {
    fn default() -> Self {
        Self::new()
    }
}
