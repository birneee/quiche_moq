use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

struct NotifyState {
    waker: Option<Waker>,
    /// Set when `notify()` is called with no task currently awaiting —
    /// the next `notified()` call resolves immediately instead of missing it.
    pending: bool,
}

/// Single-slot, executor-agnostic wake signal: one side calls `notify()` (e.g. a reader task that received
/// bytes), the other `.await`s `notified()` in a loop. One waiter at a time, unlike `tokio::sync::Notify`.
#[derive(Clone)]
pub struct Notify(Arc<Mutex<NotifyState>>);

impl Notify {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(NotifyState { waker: None, pending: false })))
    }

    /// Wake a pending `notified()` future, and mark the signal pending
    /// regardless. The latter matters even when a waker is registered: that
    /// waker might belong to a future that was polled once (e.g. by a
    /// `select!` branch that lost the race, or `FutureExt::now_or_never`)
    /// and then dropped without ever completing, in which case waking it
    /// does nothing and a future `notified()` call must still see the signal.
    pub fn notify(&self) {
        let mut state = self.0.lock().unwrap();
        state.pending = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Resolves once `notify()` has been called since the last `notified()`
    /// call (or immediately, if a notification is already pending).
    pub fn notified(&self) -> Notified<'_> {
        Notified { notify: self }
    }
}

impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.notify();
    }
}

impl Notify {
    /// Calls `notify()` when woken, e.g. by `MoqTransportSession::set_drop_waker`.
    pub fn waker(&self) -> Waker {
        Arc::new(self.clone()).into()
    }
}

impl Default for Notify {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Notified<'a> {
    notify: &'a Notify,
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.notify.0.lock().unwrap();
        if state.pending {
            state.pending = false;
            Poll::Ready(())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use futures::future::FutureExt;

    #[test]
    fn notify_before_wait_is_not_missed() {
        let n = Notify::new();
        n.notify();
        block_on(n.notified());
    }

    #[test]
    fn wait_resolves_after_notify() {
        let n = Notify::new();
        assert!(n.notified().now_or_never().is_none());
        n.notify();
        block_on(n.notified());
    }
}
