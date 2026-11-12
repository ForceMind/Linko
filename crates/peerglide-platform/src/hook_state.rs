//! State shared by the single native keyboard and mouse capture lifecycle.
use super::*;

pub(super) struct HookState {
    pub(super) pairs: PairLedger,
    pub(super) stopping: bool,
    pub(super) toggle_route: Option<CaptureRoute>,
    #[cfg(windows)]
    pub(super) local_gestures: Option<gesture_capture::CallbackBroker>,
    pub(super) tx: CaptureEventSender,
    pub(super) gate: CaptureGate,
    pub(super) keys: held_inputs::HeldKeys,
    pub(super) buttons: held_inputs::HeldButtons,
    pub(super) toggle_armed: bool,
    pub(super) target: InputCapabilities,
}
impl HookState {
    pub(super) fn keep_running(&mut self, stop: bool) -> bool {
        if !self.gate.0.healthy.load(Ordering::Acquire) {
            return false;
        }
        if stop && !self.stopping {
            self.stopping = true;
            self.toggle_armed = false;
            self.toggle_route = None;
            self.gate.disable();
        }
        !self.stopping || self.pairs.debt != 0
    }
    pub(super) fn source_lost(&mut self) {
        self.gate.source_lost();
    }
    pub(super) fn suppressed_modifiers(&self) -> bool {
        self.pairs.keys[0xe0..=0xe7].iter().any(|owner| owner.suppressed())
            // Unknown keys cannot safely be proved non-modifiers.
            || self.pairs.unknown_debt != 0
    }
    pub(super) fn physical_key_held(&self, usage: u16) -> bool {
        self.keys.contains(&usage)
            || (usize::from(usage) < self.pairs.keys.len()
                && matches!(self.pairs.keys[usize::from(usage)], PairOwner::Protected(_)))
    }
    fn finish_force_chord(&mut self) {
        if self.pairs.force_pending && !self.suppressed_modifiers() && !self.physical_key_held(0x29)
        {
            self.pairs.force_pending = false;
            self.pairs.force_armed = false;
            if let (Some(epoch), Some(control)) =
                (self.gate.receiving_epoch(), self.gate.0.receive.get())
            {
                // The second, fresh complete escape chord authorizes dropping
                // new-input protection only. Native owned debt stays Unknown.
                control.request_force_return(epoch);
            }
        }
    }
    fn protect_pair(
        &mut self,
        key: bool,
        index: usize,
        down: bool,
        route: CaptureRoute,
    ) -> Option<bool> {
        let old = if key {
            self.pairs.keys[index]
        } else {
            self.pairs.buttons[index]
        };
        if matches!(old, PairOwner::Protected(_)) {
            if !down {
                self.pairs.replace(key, index, PairOwner::Empty);
                self.update_held();
                self.finish_force_chord();
            }
            return Some(true);
        }
        let held = if key {
            self.keys.contains(&(index as u16))
        } else {
            self.buttons.contains(&(index as u8))
        };
        let protected = matches!(route, CaptureRoute::Receiving(_))
            || (!route.remote() && self.suppressed_modifiers());
        if !down || !matches!(old, PairOwner::Empty) || held || !protected {
            return None;
        }
        if key && (0xe0..=0xe7).contains(&index) && !self.suppressed_modifiers() {
            self.pairs.force_armed = self.gate.receiving_epoch().is_some_and(|epoch| {
                self.gate
                    .0
                    .receive
                    .get()
                    .is_some_and(|control| control.can_force_return(epoch))
            });
        }
        self.pairs.replace(key, index, PairOwner::Protected(route));
        self.update_held();
        self.gate.request_takeover(route);
        if key
            && index == 0x29
            && (self.physical_key_held(0xe0) || self.physical_key_held(0xe4))
            && (self.physical_key_held(0xe2) || self.physical_key_held(0xe6))
        {
            self.pairs.force_pending = self.pairs.force_armed;
            self.gate.disable();
            self.emit(Captured::Emergency);
        }
        Some(true)
    }
    fn pair_input(
        &mut self,
        event: Input,
        key: bool,
        index: usize,
        down: bool,
        route: CaptureRoute,
    ) -> bool {
        let old = if key {
            self.pairs.keys[index]
        } else {
            self.pairs.buttons[index]
        };
        let empty = matches!(old, PairOwner::Empty);
        let owner = if empty && down {
            if route.remote() {
                PairOwner::Remote(route.epoch())
            } else {
                PairOwner::Local(route.epoch())
            }
        } else if empty {
            PairOwner::Local(route.epoch())
        } else {
            old
        };
        let sent = self.emit(Captured::Input {
            event,
            route: owner.route(route),
        });
        let owner = if empty && down && owner.suppressed() && !sent {
            if self.suppressed_modifiers() {
                PairOwner::Consumed(route)
            } else {
                PairOwner::Local(route.epoch())
            }
        } else {
            owner
        };
        let suppress = owner.suppressed();
        self.pairs
            .replace(key, index, if down { owner } else { PairOwner::Empty });
        suppress
    }
    #[cfg(test)]
    pub(super) fn button(&mut self, button: u8, down: bool) -> bool {
        self.button_routed(button, down, self.gate.snapshot())
    }
    pub(super) fn button_routed(&mut self, button: u8, down: bool, route: CaptureRoute) -> bool {
        if let Some(block) = self.protect_pair(false, usize::from(button), down, route) {
            return block;
        }
        let bridge_up = button == 1
            && !down
            && matches!(self.pairs.buttons[1], PairOwner::Local(_))
            && self.gate.primary_bridge(route);
        let route = if button != 1 {
            self.stop_primary_bridge(route)
        } else {
            route
        };
        let fresh = if down {
            self.buttons.insert(button)
        } else {
            self.buttons.remove(&button);
            false
        };
        self.update_held();
        if usize::from(button) >= self.pairs.buttons.len() {
            self.gate.disable();
            self.emit(Captured::UnsupportedInput);
            return false;
        }
        if self.reject_target(Input::Button { button, down }) {
            return self.rejected_pair(false, usize::from(button), down, route);
        }
        let suppress = self.pair_input(
            Input::Button { button, down },
            false,
            usize::from(button),
            down,
            route,
        );
        if fresh && button == 1 && matches!(self.pairs.buttons[1], PairOwner::Local(_)) {
            self.gate.update_held(self.held_count(), true);
        }
        if bridge_up {
            // The original physical Down belonged to the local OS. Its Up
            // must reach that OS as well as the lease that got a bridged Down.
            self.emit(Captured::Input {
                event: Input::Button { button, down },
                route,
            });
        }
        suppress
    }
    fn stop_primary_bridge(&mut self, route: CaptureRoute) -> CaptureRoute {
        if !self.gate.primary_bridge(route) {
            return route;
        }
        self.gate.disable();
        self.emit(Captured::Emergency);
        self.gate
            .route_from(self.gate.0.control.load(Ordering::Acquire))
    }
    fn rejected_pair(&mut self, key: bool, index: usize, down: bool, route: CaptureRoute) -> bool {
        let old = if key {
            self.pairs.keys[index]
        } else {
            self.pairs.buttons[index]
        };
        let owner = if down && matches!(old, PairOwner::Empty) {
            if self.suppressed_modifiers() {
                PairOwner::Consumed(route)
            } else {
                PairOwner::Local(route.epoch())
            }
        } else {
            old
        };
        self.pairs
            .replace(key, index, if down { owner } else { PairOwner::Empty });
        owner.suppressed()
    }
    pub(super) fn unsupported_button(
        &mut self,
        button: u8,
        down: bool,
        route: CaptureRoute,
    ) -> bool {
        if let Some(block) = self.protect_pair(false, usize::from(button), down, route) {
            return block;
        }
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        self.update_held();
        self.gate.disable();
        self.toggle_armed = false;
        self.toggle_route = None;
        self.emit(Captured::UnsupportedInput);
        if usize::from(button) < self.pairs.buttons.len() {
            self.rejected_pair(false, usize::from(button), down, route)
        } else {
            false
        }
    }
    fn reject_target(&mut self, event: Input) -> bool {
        if !self.gate.active() {
            return false;
        }
        let supported = self.gate.0.routed_target.get().map_or_else(
            || self.target.supports(event),
            |target| target.supports(event),
        );
        if !supported {
            self.gate.disable();
            self.toggle_armed = false;
            self.toggle_route = None;
            self.emit(Captured::UnsupportedInput);
            true
        } else {
            false
        }
    }
    #[cfg(test)]
    pub(super) fn scroll(&mut self, x: i16, y: i16) -> bool {
        self.scroll_routed(x, y, self.gate.snapshot())
    }
    pub(super) fn scroll_routed(&mut self, x: i16, y: i16, route: CaptureRoute) -> bool {
        if matches!(route, CaptureRoute::Receiving(_))
            || (!route.remote() && self.suppressed_modifiers())
        {
            self.gate.request_takeover(route);
            return true;
        }
        if self.reject_target(Input::Scroll { x, y }) {
            return false;
        }
        if x == 0 && y == 0 {
            // The native wheel accumulator still owns the remainder. This
            // sample has nothing to forward and must not fill the queue.
            let remote = route.remote();
            if let Some(diagnostics) = self.gate.0.diagnostics.get().filter(|_| remote) {
                diagnostics.increment(input_diagnostics::Count::ScrollZero);
            }
            return remote;
        }
        self.emit(Captured::Input {
            event: Input::Scroll { x, y },
            route,
        }) && route.remote()
    }
    pub(super) fn movement(&mut self, event: Captured, route: CaptureRoute) -> bool {
        use input_diagnostics::Count;
        let remote_motion = matches!(event, Captured::Motion { .. });
        if let (Some(diagnostics), Captured::Motion { dx, dy, .. }) =
            (self.gate.0.diagnostics.get(), event)
        {
            diagnostics.increment(Count::Captured);
            if dx == 0.0 && dy == 0.0 {
                diagnostics.increment(Count::ZeroDelta);
            }
        }
        if matches!(route, CaptureRoute::Receiving(_)) {
            self.gate.request_takeover(route);
            return true;
        }
        let debt = self.pairs.mouse_debt();
        if !route.remote() && debt {
            return true;
        }
        let sent = self.emit(event);
        if remote_motion && !sent {
            if let Some(diagnostics) = self.gate.0.diagnostics.get() {
                diagnostics.increment(Count::QueueFull);
            }
        }
        route.remote() && (sent || debt)
    }
    fn held_count(&self) -> u64 {
        (self.keys.len() + self.buttons.len() + self.pairs.extra_held) as u64
    }
    pub(super) fn update_held(&self) {
        self.gate.update_held(self.held_count(), false);
    }
    pub(super) fn emit(&mut self, event: Captured) -> bool {
        if self.tx.try_push(event).is_err() {
            self.gate.disable();
            false
        } else {
            true
        }
    }
    pub(super) fn unsupported_key(&mut self, code: u16, down: bool) -> bool {
        #[cfg(windows)]
        if let Some(broker) = self.local_gestures.as_mut() {
            broker.key(0x8000 | (code & 0x7fff), down);
            return false;
        }
        let token = 0x8000 | (code & 0x7fff);
        let route = self
            .gate
            .route_from(self.gate.0.control.load(Ordering::Acquire));
        let word = usize::from(token) / 64;
        let bit = 1u64 << (token % 64);
        let consumed = self.pairs.unknown[word] & bit != 0;
        if consumed
            || (down
                && !self.keys.contains(&token)
                && (matches!(route, CaptureRoute::Receiving(_))
                    || (!route.remote() && self.suppressed_modifiers())))
        {
            if down && !consumed {
                self.pairs.unknown[word] |= bit;
                self.pairs.unknown_debt += 1;
                self.pairs.debt += 1;
                self.pairs.extra_held += 1;
            } else if !down && consumed {
                self.pairs.unknown[word] &= !bit;
                self.pairs.unknown_debt -= 1;
                self.pairs.debt -= 1;
                self.pairs.extra_held -= 1;
            }
            self.update_held();
            self.gate.request_takeover(route);
            self.finish_force_chord();
            return true;
        }
        if down {
            self.keys.insert(token);
        } else {
            self.keys.remove(&token);
        }
        self.update_held();
        self.gate.disable();
        self.toggle_armed = false;
        self.toggle_route = None;
        self.emit(Captured::UnsupportedInput);
        false
    }
    #[cfg(any(windows, test))]
    pub(super) fn key(&mut self, usage: u16, down: bool) -> bool {
        self.key_routed(usage, down, self.gate.snapshot())
    }
    pub(super) fn key_routed(&mut self, usage: u16, down: bool, route: CaptureRoute) -> bool {
        #[cfg(windows)]
        if let Some(broker) = self.local_gestures.as_mut() {
            broker.key(usage, down);
            return false;
        }
        if usize::from(usage) >= self.pairs.keys.len() {
            return self.unsupported_key(usage, down);
        }
        let route = self.stop_primary_bridge(route);
        if let Some(block) = self.protect_pair(true, usize::from(usage), down, route) {
            return block;
        }
        let fresh = if down {
            self.keys.insert(usage)
        } else {
            self.keys.remove(&usage);
            false
        };
        self.update_held();
        let ctrl = self.keys.contains(&0xe0) || self.keys.contains(&0xe4);
        let alt = self.keys.contains(&0xe2) || self.keys.contains(&0xe6);
        let old = self.pairs.keys[usize::from(usage)];
        if ctrl && alt && down && usage == 0x29 {
            self.gate.disable();
            self.toggle_armed = false;
            self.toggle_route = None;
            self.emit(Captured::Emergency);
            if matches!(old, PairOwner::Empty) {
                self.pairs
                    .replace(true, usize::from(usage), PairOwner::Consumed(route));
                return true;
            }
            return old.suppressed();
        }
        if ctrl
            && alt
            && fresh
            && usage == 0x4f
            && (self.target.motion()
                || (route.remote() && self.gate.0.routed_target.get().is_some()))
            && (route.remote() || self.pairs.debt == 0)
            && !self.stopping
        {
            self.toggle_armed = true;
            self.toggle_route = Some(route);
            self.pairs
                .replace(true, usize::from(usage), PairOwner::Consumed(route));
        }
        let owner = self.pairs.keys[usize::from(usage)];
        let suppress = if matches!(owner, PairOwner::Consumed(_)) {
            if !down {
                self.pairs
                    .replace(true, usize::from(usage), PairOwner::Empty);
            }
            true
        } else if self.reject_target(Input::Key { usage, down }) {
            self.rejected_pair(true, usize::from(usage), down, route)
        } else {
            self.pair_input(
                Input::Key { usage, down },
                true,
                usize::from(usage),
                down,
                route,
            )
        };
        if self.toggle_armed && self.keys.is_empty() {
            self.toggle_armed = false;
            if let Some(route) = self.toggle_route.take() {
                self.emit(Captured::Toggle { route });
            }
        }
        suppress
    }
}
