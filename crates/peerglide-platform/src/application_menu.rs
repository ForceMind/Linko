//! Application-menu intent, independent of native input or authorization.
//!
//! macOS owns its menu on the main thread and narrowly bridges the reviewed
//! winit 0.30.13 delegate's missing termination veto. Normal menu/Dock termination
//! requests become `Quit`; the GUI must finish its ordinary recovery before
//! closing. This does not intercept SIGKILL, Force Quit or guarantee logout.
//! Other platforms need no application-menu bridge: normal window-close handling
//! remains responsible for their shutdown.
use crate::tray::Mailbox;
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::Owner;

#[cfg(not(target_os = "macos"))]
pub struct Owner;
#[cfg(not(target_os = "macos"))]
impl Owner {
    /// No macOS-specific termination bridge is required on this platform.
    pub fn is_available(&self) -> bool {
        true
    }
}

/// Install once after the native event loop has initialized, before enabling
/// native work. Keep this owner alive independently of tray visibility. Every
/// producer must share the GUI's same stable mailbox for the owner's lifetime.
pub fn install(mailbox: Arc<Mailbox>) -> anyhow::Result<Owner> {
    #[cfg(target_os = "macos")]
    {
        macos::install(mailbox)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mailbox;
        Ok(Owner)
    }
}

#[cfg(any(target_os = "macos", test))]
const fn reviewed_winit_lock(lock: &str) -> bool {
    const fn line_is(bytes: &[u8], start: usize, end: usize, expected: &[u8]) -> bool {
        if end - start != expected.len() {
            return false;
        }
        let mut index = 0;
        while index < expected.len() {
            if bytes[start + index] != expected[index] {
                return false;
            }
            index += 1;
        }
        true
    }
    let bytes = lock.as_bytes();
    let mut offset = 0;
    let mut winit = false;
    let mut reviewed = false;
    let mut count = 0;
    while offset < bytes.len() {
        let start = offset;
        while offset < bytes.len() && bytes[offset] != b'\n' {
            offset += 1;
        }
        let mut end = offset;
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        if line_is(bytes, start, end, b"[[package]]") {
            if winit {
                if !reviewed { return false; }
                count += 1;
            }
            winit = false;
            reviewed = false;
        } else if line_is(bytes, start, end, b"name = \"winit\"") {
            winit = true;
        } else if line_is(bytes, start, end, b"version = \"0.30.13\"") {
            reviewed = true;
        }
        offset += 1;
    }
    if winit {
        if !reviewed { return false; }
        count += 1;
    }
    count == 1
}

// Compile-time check: no lockfile bytes or version parsing in the executable.
// A winit upgrade requires a fresh review of the delegate bridge before macOS
// binaries can be built. There is deliberately no new winit/build dependency.
#[cfg(target_os = "macos")]
const _: () = assert!(
    reviewed_winit_lock(include_str!("../../../Cargo.lock")),
    "Review application_menu's macOS delegate bridge before changing winit 0.30.13"
);

// Identity here only fences this process's callback owner. It is neither an
// input permission nor network authentication. The native registry is TLS.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct Registry {
    next: u64,
    current: Option<Registration>,
}
#[cfg(any(target_os = "macos", test))]
struct Registration {
    generation: u64,
    delegate: usize,
    application: usize,
    mailbox: std::sync::Weak<Mailbox>,
}
#[cfg(any(target_os = "macos", test))]
impl Registry {
    fn attach(&mut self, delegate: usize, application: usize, mailbox: &Arc<Mailbox>) -> Option<u64> {
        if self.current.is_some() {
            return None;
        }
        let generation = self.next.checked_add(1)?;
        self.next = generation;
        self.current = Some(Registration {
            generation,
            delegate,
            application,
            mailbox: Arc::downgrade(mailbox),
        });
        Some(generation)
    }
    fn mailbox(&self, delegate: usize, application: usize) -> Option<Arc<Mailbox>> {
        let current = self.current.as_ref()?;
        if current.delegate != delegate || current.application != application {
            return None;
        }
        current.mailbox.upgrade()
    }
    fn contains(&self, generation: u64) -> bool {
        self.current.as_ref().is_some_and(|entry| entry.generation == generation)
    }
    fn detach(&mut self, generation: u64) {
        if self.contains(generation) {
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::Command;

    fn mailbox() -> Arc<Mailbox> {
        Mailbox::new(Arc::new(|| {}))
    }
    #[test]
    fn bridge_routes_only_the_registered_instance_and_never_keeps_mailbox_alive() {
        let mut registry = Registry::default();
        let mailbox = mailbox();
        let owner = registry.attach(1, 2, &mailbox).unwrap();
        assert!(registry.mailbox(9, 2).is_none());
        assert!(registry.mailbox(1, 9).is_none());
        registry.mailbox(1, 2).unwrap().post(Command::Quit);
        assert!(mailbox.take().quit);
        assert!(registry.attach(1, 2, &mailbox).is_none());
        drop(mailbox);
        assert!(registry.mailbox(1, 2).is_none());
        assert!(registry.contains(owner));
        registry.detach(owner);
        assert!(!registry.contains(owner));
    }
    #[test]
    fn stale_teardown_cannot_detach_a_replacement_owner() {
        let mut registry = Registry::default();
        let first = mailbox();
        let old = registry.attach(1, 2, &first).unwrap();
        registry.detach(old);
        let replacement = mailbox();
        let new = registry.attach(1, 2, &replacement).unwrap();
        registry.detach(old);
        registry.mailbox(1, 2).unwrap().post(Command::Quit);
        assert!(replacement.take().quit);
        assert!(!first.take().quit);
        assert!(registry.contains(new));
        registry.detach(new);
        assert!(registry.mailbox(1, 2).is_none());
    }
    #[test]
    fn exhausted_generation_refuses_registration() {
        let mut registry = Registry { next: u64::MAX, current: None };
        assert!(registry.attach(1, 2, &mailbox()).is_none());
        assert!(registry.current.is_none());
    }
    #[test]
    fn reviewed_winit_version_is_not_silently_changed() {
        // The delegate bridge must be reviewed again when winit changes. This
        // test inspects the actual locked dependency, not a runtime version claim.
        assert!(reviewed_winit_lock(include_str!("../../../Cargo.lock")));
        let good = "[[package]]\nname = \"winit\"\nversion = \"0.30.13\"\n";
        assert!(reviewed_winit_lock(good));
        assert!(reviewed_winit_lock(&good.replace('\n', "\r\n")));
        assert!(!reviewed_winit_lock(&good.replace("0.30.13", "0.30.14")));
        assert!(!reviewed_winit_lock(&format!("{good}{good}")));
        assert!(!reviewed_winit_lock("[[package]]\nname = \"other\"\nversion = \"0.30.13\"\n"));
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_leave_window_close_responsible_and_do_not_post_commands() {
        let mailbox = mailbox();
        assert!(install(mailbox.clone()).unwrap().is_available());
        let pending = mailbox.take();
        assert!(!pending.show && !pending.stop && !pending.quit);
    }
}
