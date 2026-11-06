//! Local desktop recovery commands, independent of input/network authorization.
//! Native menu actions only coalesce intent and wake the GUI. The GUI must handle
//! Quit before Stop/Show, perform its normal cleanup, and never start input here.
use std::sync::{atomic::{AtomicU8, Ordering}, Arc};

const SHOW: u8 = 1;
const STOP: u8 = 2;
const QUIT: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command { Show, Stop, Quit }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending { pub show: bool, pub stop: bool, pub quit: bool }

pub struct Mailbox {
    bits: AtomicU8,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Mailbox {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Self { bits: AtomicU8::new(0), wake })
    }
    /// No bounded queue: Stop/Quit survive any number of Show notifications.
    /// Wake is a lightweight scheduling callback, never a GUI/native operation.
    pub fn post(&self, command: Command) {
        let bit = match command { Command::Show => SHOW, Command::Stop => STOP, Command::Quit => QUIT };
        if self.bits.fetch_or(bit, Ordering::AcqRel) == 0 {
            // Native action methods must never unwind across an Objective-C or
            // Win32 callback boundary. The intent stays queued if a wake panics.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.wake)()));
        }
    }
    /// One GUI owner consumes all flags atomically. Quit takes precedence in the
    /// caller; keeping all bits also preserves the associated Stop intention.
    pub fn take(&self) -> Pending {
        let bits = self.bits.swap(0, Ordering::AcqRel);
        Pending { show: bits & SHOW != 0, stop: bits & STOP != 0, quit: bits & QUIT != 0 }
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
pub use macos::Tray;
#[cfg(windows)]
pub use windows::Tray;

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Tray;
#[cfg(not(any(windows, target_os = "macos")))]
impl Tray { pub fn is_available(&self) -> bool { false } }

/// Create only local menu UI. macOS requires the application's main thread and
/// its running AppKit event loop; hold/drop Tray on that same thread.
pub fn create(mailbox: Arc<Mailbox>) -> anyhow::Result<Tray> {
    #[cfg(target_os = "macos")]
    { macos::create(mailbox) }
    #[cfg(windows)]
    { windows::create(mailbox) }
    #[cfg(not(any(windows, target_os = "macos")))]
    { let _ = mailbox; anyhow::bail!("此平台尚未提供系统托盘；请保留主窗口") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, OnceLock, Weak};

    #[test]
    fn combined_commands_wake_once_and_take_rearms_the_next_notification() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let mailbox = Mailbox::new(Arc::new(move || { count.fetch_add(1, Ordering::Relaxed); }));
        mailbox.post(Command::Show); mailbox.post(Command::Stop); mailbox.post(Command::Quit);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(mailbox.take(), Pending { show: true, stop: true, quit: true });
        assert_eq!(mailbox.take(), Pending::default());
        mailbox.post(Command::Stop);
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert!(mailbox.take().stop);
    }
    #[test]
    fn quit_and_stop_survive_full_speed_show_traffic_without_a_queue_limit() {
        let mailbox = Mailbox::new(Arc::new(|| {}));
        for _ in 0..50_000 { mailbox.post(Command::Show); }
        mailbox.post(Command::Quit); mailbox.post(Command::Stop);
        for _ in 0..50_000 { mailbox.post(Command::Show); }
        let pending = mailbox.take();
        assert!(pending.quit && pending.stop && pending.show);
        assert_eq!(mailbox.take(), Pending::default());
    }
    #[test]
    fn concurrent_producers_and_gui_drain_do_not_lose_stop_or_quit() {
        let mailbox = Mailbox::new(Arc::new(|| {}));
        let producer = mailbox.clone();
        let thread = std::thread::spawn(move || {
            for _ in 0..20_000 { producer.post(Command::Show); producer.post(Command::Stop); }
            producer.post(Command::Quit);
        });
        let mut observed = Pending::default();
        while !thread.is_finished() {
            let next = mailbox.take(); observed.stop |= next.stop; observed.quit |= next.quit;
            std::thread::yield_now();
        }
        thread.join().unwrap();
        let next = mailbox.take(); observed.stop |= next.stop; observed.quit |= next.quit;
        assert!(observed.stop && observed.quit);
    }
    #[test]
    fn wake_can_reenter_take_without_a_lock_or_reference_cycle() {
        let owner = Arc::new(OnceLock::<Weak<Mailbox>>::new());
        let callback = owner.clone();
        let seen = Arc::new(AtomicU8::new(0));
        let observed = seen.clone();
        let mailbox = Mailbox::new(Arc::new(move || {
            let mailbox = callback.get().unwrap().upgrade().unwrap();
            let next = mailbox.take();
            if next.stop { observed.fetch_or(STOP, Ordering::Relaxed); }
            if next.quit { observed.fetch_or(QUIT, Ordering::Relaxed); }
        }));
        assert!(owner.set(Arc::downgrade(&mailbox)).is_ok());
        mailbox.post(Command::Stop); mailbox.post(Command::Quit);
        assert_eq!(seen.load(Ordering::Relaxed), STOP | QUIT);
    }
}
