//! Source cursor freeze bookkeeping for an outgoing remote phase.
//!
//! Only the association request is modelled here; it never hides the cursor,
//! warps it or changes any cursor image or user setting. Callers run outside
//! native input callbacks. A disconnect the OS rejected changed nothing and is
//! not owed. A reconnect that fails stays owed, so every later local transition
//! retries it; that failure is reported but never ends the session.

/// One native association request. `true` means the OS reported success.
pub(crate) trait CursorBackend {
    fn associate(&mut self, connected: bool) -> bool;
}

/// Whether this process may have left mouse motion disconnected from the cursor.
#[derive(Default)]
pub(crate) struct FreezeState {
    owed: bool,
}

impl FreezeState {
    pub(crate) const fn new() -> Self {
        Self { owed: false }
    }

    /// Disconnect only for the exact current remote phase; once owed, repeated calls do
    /// nothing. Only a disconnect the OS confirmed has to be undone.
    pub(crate) fn freeze(&mut self, current: bool, backend: &mut impl CursorBackend) {
        if current && !self.owed {
            self.owed = backend.associate(false);
        }
    }

    /// Reconnect if owed. Returns false while the association remains unconfirmed.
    pub(crate) fn release(&mut self, backend: &mut impl CursorBackend) -> bool {
        if self.owed && backend.associate(true) {
            self.owed = false;
        }
        !self.owed
    }

    /// Periodic local-side check; never freezes, only restores outside a remote phase.
    pub(crate) fn reconcile(&mut self, remote: bool, backend: &mut impl CursorBackend) {
        if !remote {
            self.release(backend);
        }
    }

    #[cfg(test)]
    fn owed(&self) -> bool {
        self.owed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        calls: Vec<bool>,
        accept: bool,
    }
    impl Fake {
        fn new(accept: bool) -> Self {
            Self {
                calls: Vec::new(),
                accept,
            }
        }
    }
    impl CursorBackend for Fake {
        fn associate(&mut self, connected: bool) -> bool {
            self.calls.push(connected);
            self.accept
        }
    }

    #[test]
    fn remote_phase_freezes_once_and_every_end_releases_once() {
        let mut backend = Fake::new(true);
        let mut state = FreezeState::new();
        state.freeze(true, &mut backend);
        state.freeze(true, &mut backend);
        state.reconcile(true, &mut backend);
        assert_eq!(backend.calls, [false], "one disconnect per remote phase");
        assert!(state.release(&mut backend));
        assert!(state.release(&mut backend));
        state.reconcile(false, &mut backend);
        assert_eq!(backend.calls, [false, true], "release is idempotent");
        assert!(!state.owed());
    }

    #[test]
    fn stale_or_local_phase_never_disconnects() {
        let mut backend = Fake::new(true);
        let mut state = FreezeState::new();
        state.freeze(false, &mut backend);
        state.reconcile(false, &mut backend);
        assert!(state.release(&mut backend));
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn rejected_disconnect_is_not_owed_and_needs_no_reconnect() {
        let mut backend = Fake::new(false);
        let mut state = FreezeState::new();
        state.freeze(true, &mut backend);
        assert_eq!(backend.calls, [false]);
        assert!(!state.owed());
        assert!(state.release(&mut backend));
        state.reconcile(false, &mut backend);
        assert_eq!(backend.calls, [false], "nothing was disconnected");
    }

    #[test]
    fn failed_reconnect_stays_owed_until_a_local_retry_succeeds() {
        let mut backend = Fake::new(true);
        let mut state = FreezeState::new();
        state.freeze(true, &mut backend);
        backend.accept = false;
        assert!(!state.release(&mut backend));
        state.reconcile(true, &mut backend);
        assert_eq!(backend.calls, [false, true], "remote phase does not retry");
        state.reconcile(false, &mut backend);
        assert!(state.owed());
        backend.accept = true;
        state.reconcile(false, &mut backend);
        assert!(!state.owed());
        state.reconcile(false, &mut backend);
        assert_eq!(backend.calls, [false, true, true, true]);
    }
}
