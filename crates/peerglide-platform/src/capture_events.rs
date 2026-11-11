//! Bounded native capture handoff. Only the native loop wakes the async consumer.
use super::{gesture_queue, Captured};
use std::sync::Arc;
use tokio::sync::Notify;

pub struct CaptureEventSender {
    queue: Option<gesture_queue::Producer<Captured>>,
    wake: Arc<Notify>,
    #[cfg(any(windows, target_os = "macos", test))]
    dirty: bool,
}

pub struct CaptureEvents {
    queue: gesture_queue::Consumer<Captured>,
    wake: Arc<Notify>,
}

pub(super) fn channel(
    capacity: usize,
) -> Result<(CaptureEventSender, CaptureEvents), gesture_queue::InvalidCapacity> {
    let (producer, consumer) = gesture_queue::channel(capacity)?;
    let wake = Arc::new(Notify::new());
    Ok((
        CaptureEventSender {
            queue: Some(producer),
            wake: wake.clone(),
            #[cfg(any(windows, target_os = "macos", test))]
            dirty: false,
        },
        CaptureEvents {
            queue: consumer,
            wake,
        },
    ))
}

#[cfg(any(windows, target_os = "macos", test))]
impl CaptureEventSender {
    pub(super) fn try_push(&mut self, event: Captured) -> Result<(), Captured> {
        self.queue
            .as_mut()
            .expect("live capture sender owns its queue")
            .try_push(event)?;
        self.dirty = true;
        Ok(())
    }

    /// Call after returning from platform callbacks, before the loop can wait again.
    pub(super) fn notify_after_callbacks(&mut self) {
        if std::mem::take(&mut self.dirty) {
            self.wake.notify_one();
        }
    }
}

impl Drop for CaptureEventSender {
    fn drop(&mut self) {
        // Publish closure before waking the sole consumer.
        drop(self.queue.take());
        self.wake.notify_one();
    }
}

impl CaptureEvents {
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub async fn recv(&mut self) -> Option<Captured> {
        loop {
            if let Some(event) = self.queue.try_pop() {
                return Some(event);
            }
            if !self.queue.is_producer_alive() {
                // Recheck after acquiring the closure flag. A final push can
                // race the first read of the empty queue.
                return self.queue.try_pop();
            }
            self.wake.notified().await;
        }
    }

    #[cfg(test)]
    pub(super) fn try_recv(&mut self) -> Result<Captured, tokio::sync::mpsc::error::TryRecvError> {
        use tokio::sync::mpsc::error::TryRecvError;
        if let Some(event) = self.queue.try_pop() {
            return Ok(event);
        }
        if self.queue.is_producer_alive() {
            Err(TryRecvError::Empty)
        } else {
            self.queue.try_pop().ok_or(TryRecvError::Disconnected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, Wake, Waker},
    };

    #[derive(Default)]
    struct Wakes(AtomicUsize);

    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn capture_push_is_bounded_and_wakes_only_from_the_native_loop() {
        let (mut sender, mut events) = channel(1).unwrap();
        assert!(events.is_empty());
        assert!(events.try_recv().is_err());
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut next = Box::pin(events.recv());
        assert!(next.as_mut().poll(&mut cx).is_pending());

        let allocations = crate::allocation_checks::count(|| {
            assert!(sender.try_push(Captured::Emergency).is_ok());
            assert!(sender.try_push(Captured::UnsupportedInput).is_err());
        });
        assert_eq!(allocations, 0);
        assert_eq!(wakes.0.load(Ordering::Relaxed), 0);

        sender.notify_after_callbacks();
        assert_eq!(wakes.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            next.as_mut().poll(&mut cx),
            Poll::Ready(Some(Captured::Emergency))
        ));
        drop(next);
        assert!(events.is_empty());

        let mut closed = Box::pin(events.recv());
        assert!(closed.as_mut().poll(&mut cx).is_pending());
        drop(sender);
        assert_eq!(wakes.0.load(Ordering::Relaxed), 2);
        assert!(matches!(closed.as_mut().poll(&mut cx), Poll::Ready(None)));
    }
}
