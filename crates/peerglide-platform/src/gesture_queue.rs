//! Preallocated SPSC handoff. Construct and drop both endpoints outside callbacks.
//! A callback may only try_push; a full/disconnected queue returns its input.
use std::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidCapacity;

struct Inner<T: Send> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    producer_alive: AtomicBool,
    consumer_alive: AtomicBool,
}

// SAFETY: channel creates exactly one non-Clone producer and consumer. Their
// !Sync markers and &mut methods serialize each side. The producer alone writes
// an empty slot and publishes it with tail Release; the consumer's tail Acquire
// precedes its sole read. The consumer releases head only after moving T out;
// producer head Acquire precedes reuse. Slots never escape as references. Final
// destruction requires the last Arc, after all endpoint methods have ended.
// Moving a payload between these two threads requires T: Send, not T: Sync.
unsafe impl<T: Send> Sync for Inner<T> {}

/// Positions always lie in 0..slots (at most 257), so addition cannot overflow.
fn advance(position: usize, slots: usize) -> usize {
    if position + 1 == slots {
        0
    } else {
        position + 1
    }
}

/// Only final, exclusive Inner destruction owns this drain. Advance before each
/// destructor so unwinding can finish the remaining values without double-drop.
struct Drain<'a, T> {
    slots: &'a mut [UnsafeCell<MaybeUninit<T>>],
    next: usize,
    end: usize,
}
impl<T> Drain<'_, T> {
    fn drop_next(&mut self) {
        let position = self.next;
        self.next = advance(position, self.slots.len());
        // SAFETY: the final head..tail ring interval contains exactly the values
        // published but not popped. No endpoint or concurrent access remains.
        // Advancing first also excludes this value if its destructor unwinds.
        unsafe { self.slots[position].get_mut().assume_init_drop() };
    }
}
impl<T> Drop for Drain<'_, T> {
    fn drop(&mut self) {
        while self.next != self.end {
            self.drop_next();
        }
    }
}
impl<T: Send> Drop for Inner<T> {
    fn drop(&mut self) {
        // Final Arc ownership gives exclusive access, including atomic storage.
        let mut drain = Drain {
            next: *self.head.get_mut(),
            end: *self.tail.get_mut(),
            slots: &mut self.slots,
        };
        while drain.next != drain.end {
            drain.drop_next();
        }
    }
}

/// Sole producer. Send for T: Send, deliberately !Sync and not Clone.
/// Dropping may destroy queued payloads when this is the last endpoint; do that
/// outside native callbacks, even though try_push itself never allocates/blocks.
pub(crate) struct Producer<T: Send> {
    inner: Arc<Inner<T>>,
    _not_sync: PhantomData<Cell<()>>,
}
/// Sole consumer. None means empty at this observation; after producer closure,
/// published values remain available and can be drained before the final None.
pub(crate) struct Consumer<T: Send> {
    inner: Arc<Inner<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

/// Allocates capacity+1 slots once, reserving one slot to distinguish full/empty.
/// No native hook or permission is involved. Invalid capacities allocate nothing.
pub(crate) fn channel<T: Send>(
    capacity: usize,
) -> Result<(Producer<T>, Consumer<T>), InvalidCapacity> {
    if !(1..=256).contains(&capacity) {
        return Err(InvalidCapacity);
    }
    let mut slots = Vec::with_capacity(capacity + 1);
    for _ in 0..=capacity {
        slots.push(UnsafeCell::new(MaybeUninit::uninit()));
    }
    let inner = Arc::new(Inner {
        slots: slots.into_boxed_slice(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        producer_alive: AtomicBool::new(true),
        consumer_alive: AtomicBool::new(true),
    });
    Ok((
        Producer {
            inner: inner.clone(),
            _not_sync: PhantomData,
        },
        Consumer {
            inner,
            _not_sync: PhantomData,
        },
    ))
}

impl<T: Send> Producer<T> {
    /// Constant bounded work: one closure check and one full check, no retry.
    /// A push already overlapping consumer closure may finish publication; final
    /// ownership still drops it. A push observing closure returns T unchanged.
    pub(crate) fn try_push(&mut self, value: T) -> Result<(), T> {
        if !self.is_consumer_alive() {
            return Err(value);
        }
        let tail = self.inner.tail.load(Ordering::Relaxed);
        let next = advance(tail, self.inner.slots.len());
        if next == self.inner.head.load(Ordering::Acquire) {
            return Err(value);
        }
        // SAFETY: this is the sole producer, tail's slot is not in the occupied
        // interval, and acquired head ensures the previous consumer read ended.
        // It cannot be read again until the following Release publishes next.
        unsafe { self.inner.slots[tail].get().write(MaybeUninit::new(value)) };
        self.inner.tail.store(next, Ordering::Release);
        Ok(())
    }

    pub(crate) fn is_consumer_alive(&self) -> bool {
        self.inner.consumer_alive.load(Ordering::Acquire)
    }
}
impl<T: Send> Drop for Producer<T> {
    fn drop(&mut self) {
        self.inner.producer_alive.store(false, Ordering::Release);
    }
}

impl<T: Send> Consumer<T> {
    pub(crate) fn try_pop(&mut self) -> Option<T> {
        let head = self.inner.head.load(Ordering::Relaxed);
        if head == self.inner.tail.load(Ordering::Acquire) {
            return None;
        }
        let next = advance(head, self.inner.slots.len());
        // SAFETY: acquired tail publishes an initialized element at head. This
        // sole consumer moves it out exactly once; producer cannot reuse the
        // slot until the following head Release. No slot reference escapes.
        let value = unsafe { self.inner.slots[head].get().read().assume_init() };
        self.inner.head.store(next, Ordering::Release);
        Some(value)
    }

    pub(crate) fn is_producer_alive(&self) -> bool {
        self.inner.producer_alive.load(Ordering::Acquire)
    }
}
impl<T: Send> Drop for Consumer<T> {
    fn drop(&mut self) {
        self.inner.consumer_alive.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    struct Payload {
        id: usize,
        drops: Arc<Vec<AtomicUsize>>,
    }
    impl Drop for Payload {
        fn drop(&mut self) {
            self.drops[self.id].fetch_add(1, Ordering::Relaxed);
        }
    }
    fn counters(count: usize) -> Arc<Vec<AtomicUsize>> {
        Arc::new((0..count).map(|_| AtomicUsize::new(0)).collect())
    }
    fn payload(id: usize, drops: &Arc<Vec<AtomicUsize>>) -> Payload {
        Payload {
            id,
            drops: drops.clone(),
        }
    }
    fn assert_drops(drops: &Arc<Vec<AtomicUsize>>, expected: &[usize]) {
        let actual: Vec<_> = drops
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .collect();
        assert_eq!(actual.as_slice(), expected);
    }

    #[test]
    fn capacity_limits_and_fifo_hold_across_repeated_full_empty_wraps() {
        for invalid in [0, 257, usize::MAX] {
            assert!(matches!(channel::<usize>(invalid), Err(InvalidCapacity)));
        }
        for capacity in [1, 3, 256] {
            let (mut producer, mut consumer) = channel(capacity).unwrap();
            assert!(producer.is_consumer_alive() && consumer.is_producer_alive());
            assert_eq!(consumer.try_pop(), None);
            for batch in 0..20 {
                for offset in 0..capacity {
                    assert_eq!(producer.try_push(batch * capacity + offset), Ok(()));
                }
                assert_eq!(producer.try_push(usize::MAX), Err(usize::MAX));
                for offset in 0..capacity {
                    assert_eq!(consumer.try_pop(), Some(batch * capacity + offset));
                }
                assert_eq!(consumer.try_pop(), None);
            }
        }
    }

    #[test]
    fn empty_and_disconnected_are_distinct_and_closed_producer_can_be_drained() {
        let (mut producer, mut consumer) = channel(2).unwrap();
        assert_eq!(consumer.try_pop(), None);
        assert!(consumer.is_producer_alive());
        assert_eq!(producer.try_push(10), Ok(()));
        assert_eq!(producer.try_push(11), Ok(()));
        drop(producer);
        assert!(!consumer.is_producer_alive());
        assert_eq!(consumer.try_pop(), Some(10));
        assert_eq!(consumer.try_pop(), Some(11));
        assert_eq!(consumer.try_pop(), None);
    }

    #[test]
    fn send_payload_and_endpoints_do_not_require_payload_sync() {
        let (mut producer, mut consumer) = channel::<Cell<usize>>(1).unwrap();
        thread::spawn(move || {
            assert!(producer.try_push(Cell::new(7)).is_ok());
        })
        .join()
        .unwrap();
        let received = thread::spawn(move || consumer.try_pop().unwrap().get())
            .join()
            .unwrap();
        assert_eq!(received, 7);
    }

    #[test]
    fn returned_popped_and_abandoned_noncopy_values_each_drop_exactly_once() {
        let drops = counters(6);
        let (mut producer, mut consumer) = channel(3).unwrap();
        for id in 0..3 {
            assert!(producer.try_push(payload(id, &drops)).is_ok());
        }
        let rejected = producer.try_push(payload(3, &drops)).err().unwrap();
        assert_eq!(rejected.id, 3);
        assert_drops(&drops, &[0; 6]);
        drop(rejected);
        let popped = consumer.try_pop().unwrap();
        assert_eq!(popped.id, 0);
        drop(popped);
        assert!(producer.try_push(payload(4, &drops)).is_ok());
        drop(consumer);
        assert!(!producer.is_consumer_alive());
        let disconnected = producer.try_push(payload(5, &drops)).err().unwrap();
        assert_eq!(disconnected.id, 5);
        drop(disconnected);
        assert_drops(&drops, &[1, 0, 0, 1, 0, 1]);
        drop(producer);
        assert_drops(&drops, &[1; 6]);

        let drops = counters(2);
        let (mut producer, consumer) = channel(2).unwrap();
        assert!(producer.try_push(payload(0, &drops)).is_ok());
        assert!(producer.try_push(payload(1, &drops)).is_ok());
        drop(producer);
        assert_drops(&drops, &[0; 2]);
        drop(consumer);
        assert_drops(&drops, &[1; 2]);
    }

    #[test]
    fn two_threads_transfer_a_long_noncopy_sequence_in_order_without_loss_or_double_drop() {
        const COUNT: usize = 200_000;
        let drops = counters(COUNT);
        let (mut producer, mut consumer) = channel::<Payload>(17).unwrap();
        let producer_drops = drops.clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        let writer = thread::spawn(move || {
            for id in 0..COUNT {
                let mut value = payload(id, &producer_drops);
                loop {
                    match producer.try_push(value) {
                        Ok(()) => break,
                        Err(returned) => value = returned,
                    }
                    assert!(
                        producer.is_consumer_alive(),
                        "consumer disconnected during transfer"
                    );
                    assert!(Instant::now() < deadline, "producer stalled");
                    thread::yield_now();
                }
            }
        });
        let reader = thread::spawn(move || {
            for expected in 0..COUNT {
                let value = loop {
                    if let Some(value) = consumer.try_pop() {
                        break value;
                    }
                    if !consumer.is_producer_alive() {
                        // Closure Acquire orders all producer publications. Retry
                        // after observing closure rather than mistaking an earlier
                        // empty observation for loss of the final queued values.
                        break consumer
                            .try_pop()
                            .expect("producer disconnected before completion");
                    }
                    assert!(Instant::now() < deadline, "consumer stalled");
                    thread::yield_now();
                };
                assert_eq!(value.id, expected);
                drop(value);
            }
        });
        writer.join().unwrap();
        reader.join().unwrap();
        assert!(drops.iter().all(|count| count.load(Ordering::Relaxed) == 1));
    }

    #[test]
    fn consumer_disconnect_during_production_reclaims_every_constructed_payload() {
        const LIMIT: usize = 20_000;
        const READS: usize = 1000;
        let drops = counters(LIMIT);
        let producer_drops = drops.clone();
        let (mut producer, mut consumer) = channel::<Payload>(5).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let writer = thread::spawn(move || {
            for id in 0..LIMIT {
                let mut value = payload(id, &producer_drops);
                loop {
                    match producer.try_push(value) {
                        Ok(()) => break,
                        Err(returned) => value = returned,
                    }
                    if !producer.is_consumer_alive() {
                        drop(value);
                        return id + 1;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "producer failed to observe disconnect"
                    );
                    thread::yield_now();
                }
            }
            panic!("producer unexpectedly completed after the consumer stopped early");
        });
        let reader = thread::spawn(move || {
            for expected in 0..READS {
                loop {
                    if let Some(value) = consumer.try_pop() {
                        assert_eq!(value.id, expected);
                        drop(value);
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "consumer stalled before disconnect"
                    );
                    thread::yield_now();
                }
            }
            drop(consumer);
        });
        let constructed = writer.join().unwrap();
        reader.join().unwrap();
        assert!(constructed > READS && constructed <= LIMIT);
        for (id, count) in drops.iter().enumerate() {
            assert_eq!(count.load(Ordering::Relaxed), usize::from(id < constructed));
        }
    }

    #[test]
    fn final_drain_finishes_remaining_values_when_one_payload_destructor_panics() {
        struct Panicking {
            id: usize,
            drops: Arc<Vec<AtomicUsize>>,
        }
        impl Drop for Panicking {
            fn drop(&mut self) {
                self.drops[self.id].fetch_add(1, Ordering::Relaxed);
                assert_ne!(self.id, 1, "intentional destructor failure");
            }
        }
        let drops = counters(3);
        let (mut producer, consumer) = channel(3).unwrap();
        for id in 0..3 {
            assert!(producer
                .try_push(Panicking {
                    id,
                    drops: drops.clone()
                })
                .is_ok());
        }
        drop(consumer);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(producer)));
        assert!(result.is_err());
        assert_drops(&drops, &[1; 3]);
    }
}
