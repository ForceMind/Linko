//! Bounded native capture handoff. Only the native loop wakes the async consumer.
use super::{gesture_queue, CaptureEpoch, CaptureRoute, Captured};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::Notify;

pub struct CaptureEventSender {
    queue: Option<gesture_queue::Producer<Captured>>,
    #[cfg(any(windows, target_os = "macos", test))]
    position: Arc<LatestPosition>,
    wake: Arc<Notify>,
    #[cfg(any(windows, target_os = "macos", test))]
    dirty: bool,
}

pub struct CaptureEvents {
    queue: gesture_queue::Consumer<Captured>,
    position: Arc<LatestPosition>,
    seen_position: u64,
    wake: Arc<Notify>,
}

/// Separate from the reliable queue. There is one writer and one reader;
/// all accesses participate in the same atomic order. An odd revision means
/// the callback is publishing, and a changed revision invalidates the read.
#[derive(Default)]
struct LatestPosition {
    revision: AtomicU64,
    words: [AtomicU64; 6],
}

impl LatestPosition {
    #[cfg(any(windows, target_os = "macos", test))]
    fn store(&self, epoch: CaptureEpoch, x: f64, y: f64, width: f64, height: f64) {
        let odd = self.revision.load(Ordering::SeqCst).wrapping_add(1);
        self.revision.store(odd, Ordering::SeqCst);
        let words = [
            epoch.instance,
            epoch.generation,
            x.to_bits(),
            y.to_bits(),
            width.to_bits(),
            height.to_bits(),
        ];
        for (slot, word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::SeqCst);
        }
        self.revision.store(odd.wrapping_add(1), Ordering::SeqCst);
    }

    fn take(&self, seen: &mut u64) -> Option<Captured> {
        let revision = self.revision.load(Ordering::SeqCst);
        if revision == 0 || revision == *seen || revision & 1 != 0 {
            return None;
        }
        let [instance, generation, x, y, width, height] =
            self.words.each_ref().map(|word| word.load(Ordering::SeqCst));
        if self.revision.load(Ordering::SeqCst) != revision {
            return None;
        }
        *seen = revision;
        Some(Captured::Position {
            x: f64::from_bits(x),
            y: f64::from_bits(y),
            width: f64::from_bits(width),
            height: f64::from_bits(height),
            route: CaptureRoute::Local(CaptureEpoch { instance, generation }),
        })
    }
}

pub(super) fn channel(
    capacity: usize,
) -> Result<(CaptureEventSender, CaptureEvents), gesture_queue::InvalidCapacity> {
    let (producer, consumer) = gesture_queue::channel(capacity)?;
    let wake = Arc::new(Notify::new());
    let position = Arc::new(LatestPosition::default());
    Ok((
        CaptureEventSender {
            queue: Some(producer),
            #[cfg(any(windows, target_os = "macos", test))]
            position: position.clone(),
            wake: wake.clone(),
            #[cfg(any(windows, target_os = "macos", test))]
            dirty: false,
        },
        CaptureEvents {
            queue: consumer,
            position,
            seen_position: 0,
            wake,
        },
    ))
}

#[cfg(any(windows, target_os = "macos", test))]
impl CaptureEventSender {
    pub(super) fn try_push(&mut self, event: Captured) -> Result<(), Captured> {
        let queue = self
            .queue
            .as_mut()
            .expect("live capture sender owns its queue");
        if let Captured::Position {
            x, y, width, height,
            route: CaptureRoute::Local(epoch),
        } = event {
            if !queue.is_consumer_alive() {
                return Err(event);
            }
            self.position.store(epoch, x, y, width, height);
        } else {
            queue.try_push(event)?;
        }
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
            && self.position.revision.load(Ordering::SeqCst) == self.seen_position
    }

    fn try_event(&mut self) -> Option<Captured> {
        // A local position never delays a queued release, takeover or button.
        self.queue.try_pop().or_else(|| self.position.take(&mut self.seen_position))
    }

    pub async fn recv(&mut self) -> Option<Captured> {
        loop {
            if let Some(event) = self.try_event() {
                return Some(event);
            }
            if !self.queue.is_producer_alive() {
                // Recheck after acquiring the closure flag. A final push can
                // race the first read of the empty queue.
                return self.try_event();
            }
            self.wake.notified().await;
        }
    }

    #[cfg(test)]
    pub(super) fn try_recv(&mut self) -> Result<Captured, tokio::sync::mpsc::error::TryRecvError> {
        use tokio::sync::mpsc::error::TryRecvError;
        if let Some(event) = self.try_event() {
            return Ok(event);
        }
        if self.queue.is_producer_alive() {
            Err(TryRecvError::Empty)
        } else {
            self.try_event().ok_or(TryRecvError::Disconnected)
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

    fn position(index: u32) -> Captured {
        Captured::Position {
            x: f64::from(index),
            y: f64::from(index + 1),
            width: 1000.0,
            height: 800.0,
            route: CaptureRoute::Local(CaptureEpoch { instance: 1, generation: 2 }),
        }
    }

    #[test]
    fn local_position_flood_keeps_latest_without_consuming_reliable_capacity() {
        let (mut sender, mut events) = channel(1).unwrap();
        let allocations = crate::allocation_checks::count(|| {
            for index in 0..256 {
                assert!(sender.try_push(position(index)).is_ok());
            }
            assert!(sender.try_push(Captured::Emergency).is_ok());
            assert!(sender.try_push(Captured::UnsupportedInput).is_err());
        });
        assert_eq!(allocations, 0);
        assert!(matches!(events.try_recv().unwrap(), Captured::Emergency));
        assert!(!events.is_empty());
        let Captured::Position { x, y, .. } = events.try_recv().unwrap() else {
            panic!("latest position missing");
        };
        assert!((x - 255.0).abs() < f64::EPSILON);
        assert!((y - 256.0).abs() < f64::EPSILON);
        assert!(events.is_empty());
        assert!(sender.try_push(position(300)).is_ok());
        drop(sender);
        assert!(matches!(events.try_recv().unwrap(), Captured::Position { .. }));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn concurrent_reader_never_combines_two_position_samples() {
        use tokio::sync::mpsc::error::TryRecvError;

        let (mut sender, mut events) = channel(1).unwrap();
        let producer = std::thread::spawn(move || {
            for index in 0..4096 {
                assert!(sender.try_push(position(index)).is_ok());
            }
        });
        let mut last = -1.0;
        loop {
            match events.try_recv() {
                Ok(Captured::Position { x, y, .. }) => {
                    assert!(x > last);
                    assert!((y - x - 1.0).abs() < f64::EPSILON);
                    last = x;
                }
                Ok(_) => panic!("unexpected capture event"),
                Err(TryRecvError::Empty) => std::thread::yield_now(),
                Err(TryRecvError::Disconnected) => break,
            }
        }
        producer.join().unwrap();
        assert!((last - 4095.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dropped_reader_stops_position_publication() {
        let (mut sender, events) = channel(1).unwrap();
        drop(events);
        assert!(sender.try_push(position(0)).is_err());
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
