//! Event bus implement
//!
//! An Eventbus is a mechanism that allows different components to communicate with each other without knowing about each other.
use alloc::boxed::Box;
use alloc::{sync::Arc, vec::Vec};
use bitflags::bitflags;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use kernel_hal::sync::Mutex;

const MAX_EVENT_CALLBACKS: usize = 4096;

bitflags! {
    #[derive(Default)]
    /// event bus Event flags
    pub struct Event: u32 {
        /// File: is readable
        const READABLE                      = 1 << 0;
        /// File: is writeable
        const WRITABLE                      = 1 << 1;
        /// File: has error
        const ERROR                         = 1 << 2;
        /// File: is closed
        const CLOSED                        = 1 << 3;

        /// Process: is Quit
        const PROCESS_QUIT                  = 1 << 10;
        /// Process: child process is Quit
        const CHILD_PROCESS_QUIT            = 1 << 11;
        /// Process: received signal
        const RECEIVE_SIGNAL                = 1 << 12;

        /// Semaphore: is removed
        const SEMAPHORE_REMOVED             = 1 << 20;
        /// Semaphore: can acquired a resource of this semaphore
        const SEMAPHORE_CAN_ACQUIRE         = 1 << 21;
    }
}

/// handler of event in the event bus
pub type EventHandler = Box<dyn Fn(Event) -> bool + Send>;

/// event bus struct
#[derive(Default)]
pub struct EventBus {
    /// event type
    event: Event,
    /// EventBus callbacks paired with unique subscription IDs
    callbacks: Vec<(u64, EventHandler)>,
    /// counter for subscription IDs
    next_id: u64,
}

impl core::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EventBus")
            .field("event", &self.event)
            .field("callbacks_len", &self.callbacks.len())
            .finish()
    }
}

impl EventBus {
    /// create an event bus
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }

    /// set event flag
    pub fn set(&mut self, set: Event) {
        self.change(Event::empty(), set);
    }

    /// clear all event flag
    pub fn clear(&mut self, set: Event) {
        self.change(set, Event::empty());
    }

    /// change event flag
    /// - `reset`: flag to remove
    /// - `set`: flag to insert
    pub fn change(&mut self, reset: Event, set: Event) {
        let orig = self.event;
        let mut new = self.event;
        new.remove(reset);
        new.insert(set);
        self.event = new;
        if new != orig {
            let pending = core::mem::take(&mut self.callbacks);
            let mut kept = Vec::with_capacity(pending.len());
            for (id, f) in pending {
                if !f(new) {
                    kept.push((id, f));
                }
            }
            let mut late = core::mem::take(&mut self.callbacks);
            kept.append(&mut late);
            self.callbacks = kept;
        }
    }

    /// The currently set event flags.
    pub fn events(&self) -> Event {
        self.event
    }

    /// push a EventHandler into the callback vector, returning a subscription ID if registered
    pub fn subscribe(&mut self, callback: EventHandler) -> Option<u64> {
        if !self.event.is_empty() && callback(self.event) {
            return None;
        }
        if self.callbacks.len() >= MAX_EVENT_CALLBACKS {
            let (_id, oldest) = self.callbacks.remove(0);
            let _ = oldest(self.event);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.callbacks.push((id, callback));
        Some(id)
    }

    /// Unsubscribe a previously registered callback by its ID.
    pub fn unsubscribe(&mut self, id: u64) {
        self.callbacks.retain(|(item_id, _)| *item_id != id);
    }

    /// get the callback vector length
    pub fn get_callback_len(&self) -> usize {
        self.callbacks.len()
    }
}

/// RAII handle for a waker parked on some file's event source.
pub struct ReadinessSub {
    unsub: Option<alloc::boxed::Box<dyn FnOnce() + Send>>,
}

impl ReadinessSub {
    /// A subscription whose cleanup runs `unsub` on drop.
    pub fn new(unsub: alloc::boxed::Box<dyn FnOnce() + Send>) -> Self {
        Self { unsub: Some(unsub) }
    }

    /// A subscription with nothing to clean up.
    pub fn noop() -> Self {
        Self { unsub: None }
    }
}

impl Drop for ReadinessSub {
    fn drop(&mut self) {
        if let Some(f) = self.unsub.take() {
            f();
        }
    }
}

/// Park `waker` on `bus` as a one-shot callback for any event in `mask`.
pub fn subscribe_waker(bus: &mut EventBus, mask: Event, waker: &core::task::Waker) -> Option<u64> {
    let waker = waker.clone();
    bus.subscribe(Box::new(move |events| {
        if (events & mask).is_empty() {
            return false;
        }
        waker.wake_by_ref();
        true
    }))
}

/// [`subscribe_waker`] + RAII handle for `Arc<Mutex<EventBus>>`.
pub fn subscribe_readiness_on(
    bus: &Arc<Mutex<EventBus>>,
    mask: Event,
    waker: &core::task::Waker,
) -> ReadinessSub {
    match subscribe_waker(&mut bus.lock(), mask, waker) {
        Some(id) => {
            let bus = bus.clone();
            ReadinessSub::new(Box::new(move || {
                bus.lock().unsubscribe(id);
            }))
        }
        None => ReadinessSub::noop(),
    }
}

/// wait for a event async
pub fn wait_for_event(bus: Arc<Mutex<EventBus>>, mask: Event) -> impl Future<Output = Event> {
    EventBusFuture {
        bus,
        mask,
        sub_id: None,
    }
}

/// EventBus future for async
#[must_use = "future does nothing unless polled/`await`-ed"]
struct EventBusFuture {
    bus: Arc<Mutex<EventBus>>,
    mask: Event,
    sub_id: Option<u64>,
}

impl Drop for EventBusFuture {
    fn drop(&mut self) {
        if let Some(id) = self.sub_id.take() {
            self.bus.lock().unsubscribe(id);
        }
    }
}

impl Future for EventBusFuture {
    type Output = Event;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let mut lock = this.bus.lock();
        if !(lock.event & this.mask).is_empty() {
            if let Some(id) = this.sub_id.take() {
                lock.unsubscribe(id);
            }
            return Poll::Ready(lock.event);
        }
        if this.sub_id.is_none() {
            let waker = cx.waker().clone();
            let mask = this.mask;
            let sub_id = lock.subscribe(Box::new(move |s| {
                if (s & mask).is_empty() {
                    return false;
                }
                waker.wake_by_ref();
                true
            }));
            this.sub_id = sub_id;
        }
        Poll::Pending
    }
}
