//! Contains the `Broadcast` type, which is basically just a wrapper around `embassy_sync::pubsub::PubSubChannel` but specifically meant for allowing tasks to subscribe to an awaitable trigger signal.
//!
//! u_TODO - if actually keep using this then this should probably be a standalone crate (maybe in firmware-rs repo) so it can be reused

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    pubsub::{PubSubChannel, Subscriber},
};

/// Errors that can occur when trying to wait for a broadcast signal.
#[derive(Debug)]
pub enum WaitError {
    /// The maximum number of waiters has been reached, so the task cannot wait for the signal right now. This means you should probably increase the `N` of the `Broadcast`.
    MaxWaitersReached,
}

/// N is the max number of waiters this broadcast should allow.
pub struct Broadcast<M: RawMutex, const N: usize> {
    inner: PubSubChannel<M, (), 1, N, 0>,
}
impl<M: RawMutex, const N: usize> Default for Broadcast<M, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex, const N: usize> Broadcast<M, N> {
    pub const fn new() -> Self {
        Self { inner: PubSubChannel::new() }
    }

    /// Subscribes you to this broadcast. This doesn't automatically do anything on its own, but it gives you the `Subscription` handle (which allows you to .wait() on this broadcast).
    ///
    /// This is meant to be called at the init stage of tasks that want to be notified based
    /// on this broadcast.
    pub fn subscribe(&self) -> Result<Subscription<'_, M, N>, WaitError> {
        let subscriber = self.inner.subscriber().map_err(|_| WaitError::MaxWaitersReached)?;

        Ok(Subscription { subscriber })
    }

    /// Dispatch a broadcast signal. This will wake up all tasks currently waiting on this broadcast.
    pub fn signal(&self) {
        self.inner.immediate_publisher().publish_immediate(());
    }
}

/// A registered subscription for a `Broadcast`.
pub struct Subscription<'broadcast, M: RawMutex, const N: usize> {
    subscriber: Subscriber<'broadcast, M, (), 1, N, 0>,
}
impl<M: RawMutex, const N: usize> Subscription<'_, M, N> {
    /// Waits until the broadcast is signaled.
    ///
    /// If a broadcast has already been signaled since you last called this function, this future will resolve immediately.
    /// If you want to throw away any signals that arrived since you last checked, and just get woken up exactly whenever the NEXT signal comes in, you can call `clear()` before `wait()`.
    pub async fn wait(&mut self) {
        self.subscriber.next_message_pure().await;
    }

    /// Throws away any signals that fired since the last `wait()`, so the next `wait()` only resolves on a signal that fires from here on out.
    pub fn clear(&mut self) {
        while self.subscriber.try_next_message_pure().is_some() {}
    }
}
