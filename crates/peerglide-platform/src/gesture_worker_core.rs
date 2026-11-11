//! Worker-only composition of the existing G03 model, recognizer and exact rules.
//! No native hooks, input effects, permission decisions or target authentication.
//! The host must cancel/return a pending Prepared before dropping this core.
use crate::gesture_capture::{CoordinateUnits, RawPoint};
use peerglide_actions::{
    rules::{CompiledRules, Resolution, RuleBook, RuleError},
    ActionSpec, Context,
};
use peerglide_core::gesture_broker::{
    Completion, Delivery, Environment, Event, GestureBroker, Intent, Origin, Phase,
};
use peerglide_gestures::{Point, Recognizer};
use std::time::{Duration, Instant};

pub(super) struct Begin<P> {
    pub(super) sequence: u64,
    pub(super) prepared: P,
    pub(super) context: Context,
    pub(super) app: String,
    pub(super) dpi: u32,
    pub(super) point: RawPoint,
    pub(super) at: Instant,
    pub(super) expires: Instant,
}

struct Pending<P> {
    sequence: u64,
    prepared: P,
    environment: Environment,
    app: String,
    first: RawPoint,
    dpi: u32,
    started: Instant,
    expires: Instant,
    last_at: Instant,
}

pub(super) enum Finish<P, R> {
    Click {
        sequence: u64,
        prepared: P,
        receipt: R,
    },
    Action {
        sequence: u64,
        prepared: P,
        spec: ActionSpec,
    },
    Cancelled {
        sequence: u64,
        prepared: P,
    },
}

pub(super) struct Core<'a, P> {
    recognizer: &'a Recognizer,
    broker: GestureBroker<'a>,
    rules: CompiledRules,
    pending: Option<Pending<P>>,
}

fn valid_raw(point: RawPoint) -> bool {
    point.units == CoordinateUnits::WindowsPhysicalPixels
        && point.x.is_finite()
        && point.y.is_finite()
        && point.x.abs() <= 1_000_000.0
        && point.y.abs() <= 1_000_000.0
}

impl<P> Pending<P> {
    fn elapsed(&self, at: Instant, maximum: Duration) -> Option<Duration> {
        let elapsed = at.checked_duration_since(self.started)?;
        if at < self.last_at || at >= self.expires || elapsed >= maximum {
            return None;
        }
        Some(elapsed)
    }

    fn sample(
        &self,
        sequence: u64,
        raw: RawPoint,
        at: Instant,
        maximum: Duration,
    ) -> Option<(Point, Duration)> {
        if sequence != self.sequence || !valid_raw(raw) {
            return None;
        }
        let elapsed = self.elapsed(at, maximum)?;
        let scale = 96.0 / f64::from(self.dpi);
        let point = Point::new(
            (raw.x - self.first.x) * scale,
            (raw.y - self.first.y) * scale,
        );
        // Raw bounds and frozen valid DPI make conversion finite. The existing
        // stroke collector, not this wrapper, enforces logical-coordinate/path
        // limits and rejects the entire candidate on an invalid/long trajectory.
        if !point.x.is_finite() || !point.y.is_finite() {
            return None;
        }
        Some((point, elapsed))
    }
}

impl<'a, P> Core<'a, P> {
    pub(super) fn new(recognizer: &'a Recognizer, book: RuleBook) -> Result<Self, RuleError> {
        Ok(Self {
            recognizer,
            broker: GestureBroker::new(recognizer),
            rules: CompiledRules::new(book)?,
            pending: None,
        })
    }

    /// Pure rule eligibility only. The host separately authenticates the native
    /// target and checks independent consent, capability and the original lease.
    pub(super) fn eligible(&self, app: &str, allow: impl Fn(ActionSpec) -> bool) -> bool {
        self.recognizer
            .template_ids()
            .any(|id| match self.rules.resolve(id.0, app) {
                Resolution::Selected { action, .. } => allow(action),
                _ => false,
            })
    }

    pub(super) fn sequence(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.sequence)
    }

    pub(super) fn prepared(&self) -> Option<&P> {
        self.pending.as_ref().map(|pending| &pending.prepared)
    }

    pub(super) fn begin(&mut self, input: Begin<P>) -> Vec<(u64, P)> {
        let mut cancelled = Vec::with_capacity(2);
        if let Some(old) = self.cancel() {
            cancelled.push(old);
        }
        let Begin {
            sequence,
            prepared,
            context,
            app,
            dpi,
            point,
            at,
            expires,
        } = input;
        if !(48..=768).contains(&dpi)
            || !valid_raw(point)
            || expires <= at
            || !self.eligible(&app, |_| true)
        {
            cancelled.push((sequence, prepared));
            return cancelled;
        }
        let environment = Environment {
            eligible: true,
            remote_target: false,
            context,
        };
        let effect = self.broker.handle(
            Event::RightDown(Point::new(0.0, 0.0)),
            Origin::PhysicalLocal,
            environment,
            Duration::ZERO,
        );
        if effect.delivery != Delivery::SuppressRightButton
            || !matches!(self.broker.phase(), Phase::Pending | Phase::Tracking)
            || effect.completion.is_some()
        {
            self.broker = GestureBroker::new(self.recognizer);
            cancelled.push((sequence, prepared));
            return cancelled;
        }
        self.pending = Some(Pending {
            sequence,
            prepared,
            environment,
            app,
            first: point,
            dpi,
            started: at,
            expires,
            last_at: at,
        });
        cancelled
    }

    pub(super) fn motion(
        &mut self,
        sequence: u64,
        point: RawPoint,
        at: Instant,
    ) -> Option<(u64, P)> {
        let pending = self.pending.as_ref()?;
        let Some((point, elapsed)) =
            pending.sample(sequence, point, at, self.recognizer.config().max_duration)
        else {
            return self.cancel();
        };
        let environment = pending.environment;
        let effect = self.broker.handle(
            Event::Move(point),
            Origin::PhysicalLocal,
            environment,
            elapsed,
        );
        if effect.completion.is_some()
            || !matches!(self.broker.phase(), Phase::Pending | Phase::Tracking)
        {
            return self.cancel();
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.last_at = at;
        }
        None
    }

    pub(super) fn end<R>(
        &mut self,
        sequence: u64,
        point: RawPoint,
        at: Instant,
        receipt: R,
    ) -> Option<Finish<P, R>> {
        let pending = self.pending.as_ref()?;
        let Some((point, elapsed)) =
            pending.sample(sequence, point, at, self.recognizer.config().max_duration)
        else {
            return self
                .cancel()
                .map(|(sequence, prepared)| Finish::Cancelled { sequence, prepared });
        };
        let environment = pending.environment;
        let completion = self
            .broker
            .handle(
                Event::RightUp(point),
                Origin::PhysicalLocal,
                environment,
                elapsed,
            )
            .completion;
        let pending = self.pending.take()?;
        // Worker cancellation must not leave CancelledHeld waiting for an up
        // which the native broker already suppressed or stopped delivering.
        self.broker = GestureBroker::new(self.recognizer);
        let intent = match completion {
            Some(Completion::Ready(ticket)) => ticket.check(pending.environment, elapsed).ok(),
            _ => None,
        };
        let Pending {
            sequence,
            prepared,
            app,
            ..
        } = pending;
        Some(match intent {
            Some(Intent::BalancedRightClick(_)) => Finish::Click {
                sequence,
                prepared,
                receipt,
            },
            Some(Intent::Gesture { matched, .. }) => match self.rules.resolve(matched.id.0, &app) {
                Resolution::Selected { action, .. } => Finish::Action {
                    sequence,
                    prepared,
                    spec: action,
                },
                _ => Finish::Cancelled { sequence, prepared },
            },
            _ => Finish::Cancelled { sequence, prepared },
        })
    }

    pub(super) fn cancel(&mut self) -> Option<(u64, P)> {
        self.broker.cancel();
        self.broker = GestureBroker::new(self.recognizer);
        self.pending
            .take()
            .map(|pending| (pending.sequence, pending.prepared))
    }

    pub(super) fn tick(&mut self, now: Instant, cancelled_through: u64) -> Option<(u64, P)> {
        let pending = self.pending.as_ref()?;
        if pending.sequence <= cancelled_through
            || pending
                .elapsed(now, self.recognizer.config().max_duration)
                .is_none()
        {
            return self.cancel();
        }
        None
    }

    pub(super) fn replace_rules(&mut self, book: RuleBook) -> Result<Option<(u64, P)>, RuleError> {
        let compiled = CompiledRules::new(book)?;
        let cancelled = self.cancel();
        self.rules = compiled;
        Ok(cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_actions::{
        rules::{Rule, Scope},
        Action,
    };
    use peerglide_gestures::{starter_templates, Config, GestureId, Template, MAX_SAMPLES};
    use std::{cell::RefCell, rc::Rc};

    // This move-only value is only a pure ownership probe, never native consent,
    // a window handle, an action capability or a process-authentication token.
    struct Prepared {
        id: u64,
        dropped: Rc<RefCell<Vec<u64>>>,
    }
    struct Receipt(u64);
    impl Drop for Prepared {
        fn drop(&mut self) {
            self.dropped.borrow_mut().push(self.id);
        }
    }
    type Drops = Rc<RefCell<Vec<u64>>>;
    fn prepared(id: u64, drops: &Drops) -> Prepared {
        Prepared {
            id,
            dropped: drops.clone(),
        }
    }
    fn recognizer() -> Recognizer {
        Recognizer::new(Config::default(), starter_templates()).unwrap()
    }
    fn rule(id: u64, gesture: u32, scope: Scope, action: Action) -> Rule {
        Rule {
            id,
            enabled: true,
            gesture,
            scope,
            priority: 0,
            action: ActionSpec::new(action),
        }
    }
    fn book() -> RuleBook {
        RuleBook {
            enabled: true,
            rules: vec![rule(1, 2, Scope::Global, Action::MinimizeWindow)],
            ..RuleBook::default()
        }
    }
    fn context() -> Context {
        Context {
            device: [1; 32],
            target: [2; 32],
            focus_generation: 3,
            configuration_generation: 4,
            trigger_sequence: 5,
        }
    }
    fn point(x: f64, y: f64) -> RawPoint {
        RawPoint {
            x,
            y,
            units: CoordinateUnits::WindowsPhysicalPixels,
        }
    }
    fn begin(id: u64, at: Instant, drops: &Drops) -> Begin<Prepared> {
        Begin {
            sequence: id,
            prepared: prepared(id, drops),
            context: context(),
            app: "app:Editor".into(),
            dpi: 96,
            point: point(1000.0, -500.0),
            at,
            expires: at.checked_add(Duration::from_secs(5)).unwrap(),
        }
    }
    fn cancelled(result: Option<(u64, Prepared)>, expected: u64) {
        let (sequence, prepared) = result.expect("the host must receive its pending Prepared");
        assert_eq!(sequence, expected);
        assert_eq!(prepared.id, expected);
        drop(prepared);
    }
    fn cancelled_end(result: Option<Finish<Prepared, ()>>, expected: u64) {
        match result {
            Some(Finish::Cancelled { sequence, prepared }) => {
                assert_eq!(sequence, expected);
                assert_eq!(prepared.id, expected);
                drop(prepared);
            }
            _ => panic!("expected returned cancelled Prepared"),
        }
    }
    fn action(result: Option<Finish<Prepared, ()>>, expected: u64, expected_action: Action) {
        match result {
            Some(Finish::Action {
                sequence,
                prepared,
                spec,
            }) => {
                assert_eq!(sequence, expected);
                assert_eq!(prepared.id, expected);
                assert_eq!(spec, ActionSpec::new(expected_action));
                drop(prepared);
            }
            _ => panic!("expected a recognized, mapped action"),
        }
    }

    #[test]
    fn model_distinguishes_short_click_action_and_rejection_without_fallback() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        for (sequence, elapsed, click) in [(1, 400, true), (2, 401, false)] {
            assert!(core.begin(begin(sequence, now, &drops)).is_empty());
            let result = core.end(
                sequence,
                point(1001.0, -500.0),
                now + Duration::from_millis(elapsed),
                Receipt(sequence + 100),
            );
            match (click, result) {
                (
                    true,
                    Some(Finish::Click {
                        sequence: actual,
                        prepared,
                        receipt,
                    }),
                ) => {
                    assert_eq!(actual, sequence);
                    assert_eq!(prepared.id, sequence);
                    assert_eq!(receipt.0, sequence + 100);
                    drop(prepared);
                }
                (
                    false,
                    Some(Finish::Cancelled {
                        sequence: actual,
                        prepared,
                    }),
                ) => {
                    assert_eq!(actual, sequence);
                    assert_eq!(prepared.id, sequence);
                    drop(prepared);
                }
                _ => panic!("click threshold must come from the existing model"),
            }
            assert!(core.sequence().is_none() && core.prepared().is_none());
        }
        assert!(core.begin(begin(3, now, &drops)).is_empty());
        assert!(core
            .motion(3, point(1060.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        action(
            core.end(
                3,
                point(1100.0, -500.0),
                now + Duration::from_millis(200),
                (),
            ),
            3,
            Action::MinimizeWindow,
        );
        assert!(core.begin(begin(4, now, &drops)).is_empty());
        assert!(core
            .motion(4, point(1060.0, -440.0), now + Duration::from_millis(100))
            .is_none());
        cancelled_end(
            core.end(
                4,
                point(1100.0, -400.0),
                now + Duration::from_millis(200),
                (),
            ),
            4,
        );
        // Recognized left has no mapping; never reinterpret it as a plain click.
        assert!(core.begin(begin(5, now, &drops)).is_empty());
        assert!(core
            .motion(5, point(940.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        cancelled_end(
            core.end(
                5,
                point(900.0, -500.0),
                now + Duration::from_millis(200),
                (),
            ),
            5,
        );
        assert_eq!(*drops.borrow(), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn eligibility_respects_exact_app_exclusion_disabled_and_host_allowance() {
        let recognizer = recognizer();
        let mut exact = book();
        exact.rules[0].scope = Scope::Application {
            id: "app:Editor".into(),
        };
        let core: Core<'_, ()> = Core::new(&recognizer, exact.clone()).unwrap();
        assert!(core.eligible("app:Editor", |spec| spec.action == Action::MinimizeWindow));
        assert!(!core.eligible("app:editor", |_| true));
        assert!(!core.eligible("app:Other", |_| true));
        assert!(!core.eligible("app:Editor", |_| false));
        for invalid in ["", " leading", "trailing ", "app:bad\nvalue"] {
            assert!(!core.eligible(invalid, |_| true));
        }
        exact.exclusions.push("app:Editor".into());
        let excluded: Core<'_, ()> = Core::new(&recognizer, exact).unwrap();
        assert!(!excluded.eligible("app:Editor", |_| true));
        let mut disabled = book();
        disabled.enabled = false;
        let mut core = Core::new(&recognizer, disabled).unwrap();
        let drops = Drops::default();
        let returned = core.begin(begin(1, Instant::now(), &drops));
        assert_eq!(returned.len(), 1);
        assert!(core.sequence().is_none());
        assert!(drops.borrow().is_empty());
        drop(returned);
        assert_eq!(*drops.borrow(), [1]);
    }

    #[test]
    fn actual_custom_template_ids_determine_eligibility_and_action_resolution() {
        let recognizer = Recognizer::new(
            Config::default(),
            vec![Template {
                id: GestureId(37),
                points: vec![Point::new(0.0, 0.0), Point::new(100.0, 0.0)],
            }],
        )
        .unwrap();
        let mut custom = book();
        custom.rules[0].gesture = 37;
        let mut core = Core::new(&recognizer, custom).unwrap();
        assert!(core.eligible("app:Editor", |_| true));
        let drops = Drops::default();
        let now = Instant::now();
        assert!(core.begin(begin(1, now, &drops)).is_empty());
        assert!(core
            .motion(1, point(1060.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        action(
            core.end(
                1,
                point(1100.0, -500.0),
                now + Duration::from_millis(200),
                (),
            ),
            1,
            Action::MinimizeWindow,
        );
        let absent: Core<'_, ()> = Core::new(&recognizer, book()).unwrap();
        assert!(!absent.eligible("app:Editor", |_| true));
    }

    #[test]
    fn translated_physical_paths_have_the_same_logical_result_at_all_supported_dpi() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        for (index, dpi) in [48, 96, 144, 192, 768].into_iter().enumerate() {
            let sequence = index as u64 + 1;
            let mut input = begin(sequence, now, &drops);
            input.dpi = dpi;
            input.point = point(-900_000.0, 700_000.0);
            assert!(core.begin(input).is_empty());
            let scale = f64::from(dpi) / 96.0;
            assert!(core
                .motion(
                    sequence,
                    point(-900_000.0 + 60.0 * scale, 700_000.0),
                    now + Duration::from_millis(100)
                )
                .is_none());
            action(
                core.end(
                    sequence,
                    point(-900_000.0 + 100.0 * scale, 700_000.0),
                    now + Duration::from_millis(200),
                    (),
                ),
                sequence,
                Action::MinimizeWindow,
            );
        }
        assert_eq!(drops.borrow().len(), 5);
    }

    #[test]
    fn begin_replacement_and_invalid_new_input_return_every_prepared_before_drop() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        assert!(core.begin(begin(1, now, &drops)).is_empty());
        assert_eq!(core.prepared().unwrap().id, 1);
        let mut invalid = begin(2, now, &drops);
        invalid.dpi = 0;
        let returned = core.begin(invalid);
        assert_eq!(returned.len(), 2);
        assert_eq!((returned[0].0, returned[0].1.id), (1, 1));
        assert_eq!((returned[1].0, returned[1].1.id), (2, 2));
        assert!(drops.borrow().is_empty());
        assert!(core.sequence().is_none());
        drop(returned);
        assert!(core.begin(begin(3, now, &drops)).is_empty());
        cancelled(core.cancel(), 3);
        assert!(core.cancel().is_none());
        // Rebuilding the worker-only model lets a new down begin without an old
        // native up being replayed solely to escape CancelledHeld.
        assert!(core.begin(begin(4, now, &drops)).is_empty());
        match core.end(
            4,
            point(1000.0, -500.0),
            now + Duration::from_millis(10),
            (),
        ) {
            Some(Finish::Click { prepared, .. }) => drop(prepared),
            _ => panic!("cancelled model must not trap the next native stroke"),
        }
        assert!(core.begin(begin(5, now, &drops)).is_empty());
        let returned = core.begin(begin(6, now, &drops));
        assert_eq!(returned.len(), 1);
        assert_eq!((returned[0].0, returned[0].1.id), (5, 5));
        assert_eq!(core.sequence(), Some(6));
        assert_eq!(core.prepared().unwrap().id, 6);
        assert_eq!(*drops.borrow(), [1, 2, 3, 4]);
        drop(returned);
        cancelled(core.cancel(), 6);
        assert_eq!(*drops.borrow(), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn invalid_rules_preserve_live_pending_and_valid_replacement_returns_it() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        assert!(core.begin(begin(1, now, &drops)).is_empty());
        let mut bad = book();
        bad.schema_version = 2;
        assert!(matches!(core.replace_rules(bad), Err(RuleError::Schema)));
        let mut conflict = book();
        let mut duplicate = conflict.rules[0].clone();
        duplicate.id = 2;
        conflict.rules.push(duplicate);
        assert!(matches!(
            core.replace_rules(conflict),
            Err(RuleError::Conflict)
        ));
        assert_eq!(core.sequence(), Some(1));
        assert_eq!(core.prepared().unwrap().id, 1);
        assert!(drops.borrow().is_empty());
        assert!(core
            .motion(1, point(1060.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        action(
            core.end(
                1,
                point(1100.0, -500.0),
                now + Duration::from_millis(200),
                (),
            ),
            1,
            Action::MinimizeWindow,
        );
        assert!(core.begin(begin(2, now, &drops)).is_empty());
        let mut changed = book();
        changed.rules[0].action = ActionSpec::new(Action::MaximizeWindow);
        cancelled(core.replace_rules(changed).unwrap(), 2);
        assert!(core.begin(begin(3, now, &drops)).is_empty());
        assert!(core
            .motion(3, point(1060.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        action(
            core.end(
                3,
                point(1100.0, -500.0),
                now + Duration::from_millis(200),
                (),
            ),
            3,
            Action::MaximizeWindow,
        );
        assert_eq!(*drops.borrow(), [1, 2, 3]);
    }

    #[test]
    fn malformed_units_coordinates_dpi_or_expiry_return_the_new_prepared() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        for id in 1..=8 {
            let mut input = begin(id, now, &drops);
            match id {
                1 => input.point.units = CoordinateUnits::MacDisplayPoints,
                2 => input.point.x = f64::NAN,
                3 => input.point.y = f64::INFINITY,
                4 => input.point.x = f64::MAX,
                5 => input.dpi = 47,
                6 => input.dpi = 769,
                7 => input.expires = now,
                _ => input.expires = now.checked_sub(Duration::from_nanos(1)).unwrap(),
            }
            let returned = core.begin(input);
            assert_eq!(returned.len(), 1);
            assert_eq!(returned[0].1.id, id);
            assert!(core.prepared().is_none());
            drop(returned);
        }
        assert_eq!(*drops.borrow(), (1..=8).collect::<Vec<_>>());
    }

    #[test]
    fn bad_sequence_point_or_clock_cancels_original_prepared_and_end_never_loses_it() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        assert!(core.begin(begin(1, now, &drops)).is_empty());
        cancelled(core.motion(99, point(1010.0, -500.0), now), 1);
        assert!(core.begin(begin(2, now, &drops)).is_empty());
        cancelled_end(core.end(99, point(1000.0, -500.0), now, ()), 2);
        assert!(core.begin(begin(3, now, &drops)).is_empty());
        assert!(core
            .motion(3, point(1010.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        cancelled(
            core.motion(3, point(1020.0, -500.0), now + Duration::from_millis(99)),
            3,
        );
        assert!(core.begin(begin(4, now, &drops)).is_empty());
        cancelled_end(
            core.end(
                4,
                point(1000.0, -500.0),
                now.checked_sub(Duration::from_nanos(1)).unwrap(),
                (),
            ),
            4,
        );
        assert!(core.begin(begin(5, now, &drops)).is_empty());
        cancelled(core.motion(5, point(f64::NAN, -500.0), now), 5);
        assert!(core.begin(begin(6, now, &drops)).is_empty());
        let mut wrong_units = point(1000.0, -500.0);
        wrong_units.units = CoordinateUnits::MacDisplayPoints;
        cancelled_end(core.end(6, wrong_units, now, ()), 6);
        assert!(core.end(6, point(1000.0, -500.0), now, ()).is_none());
        assert!(core.begin(begin(7, now, &drops)).is_empty());
        assert!(core
            .motion(7, point(1010.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        cancelled_end(
            core.end(
                7,
                point(1020.0, -500.0),
                now + Duration::from_millis(99),
                (),
            ),
            7,
        );
        assert!(core.begin(begin(8, now, &drops)).is_empty());
        assert!(core
            .motion(8, point(1010.0, -500.0), now + Duration::from_millis(100))
            .is_none());
        cancelled(core.tick(now + Duration::from_millis(99), 0), 8);
        assert_eq!(*drops.borrow(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn tick_and_end_enforce_original_expiry_model_duration_clock_and_cancel_watermark() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        assert!(core.begin(begin(1, now, &drops)).is_empty());
        assert!(core.tick(now + Duration::from_millis(10), 0).is_none());
        cancelled(core.tick(now + recognizer.config().max_duration, 0), 1);
        let mut expiring = begin(2, now, &drops);
        expiring.expires = now + Duration::from_millis(50);
        assert!(core.begin(expiring).is_empty());
        cancelled_end(
            core.end(
                2,
                point(1000.0, -500.0),
                now + Duration::from_millis(50),
                (),
            ),
            2,
        );
        let mut expiring = begin(3, now, &drops);
        expiring.expires = now + Duration::from_millis(50);
        assert!(core.begin(expiring).is_empty());
        cancelled(core.tick(now + Duration::from_millis(50), 0), 3);
        assert!(core.begin(begin(4, now, &drops)).is_empty());
        cancelled(
            core.tick(now.checked_sub(Duration::from_nanos(1)).unwrap(), 0),
            4,
        );
        assert!(core.begin(begin(5, now, &drops)).is_empty());
        cancelled(core.tick(now, 5), 5);
        assert!(core.begin(begin(6, now, &drops)).is_empty());
        cancelled_end(
            core.end(
                6,
                point(1000.0, -500.0),
                now + recognizer.config().max_duration,
                (),
            ),
            6,
        );
        assert!(core.begin(begin(u64::MAX, now, &drops)).is_empty());
        assert!(core.tick(now, u64::MAX - 1).is_none());
        cancelled(core.tick(now, u64::MAX), u64::MAX);
        assert!(core.tick(now, u64::MAX).is_none());
        assert_eq!(*drops.borrow(), [1, 2, 3, 4, 5, 6, u64::MAX]);
    }

    #[test]
    fn logical_coordinate_or_sample_budget_overflow_cancels_without_click_fallback() {
        let recognizer = recognizer();
        let drops = Drops::default();
        let mut core = Core::new(&recognizer, book()).unwrap();
        let now = Instant::now();
        let mut wide = begin(1, now, &drops);
        wide.dpi = 48;
        wide.point = point(-1_000_000.0, 0.0);
        assert!(core.begin(wide).is_empty());
        cancelled(
            core.motion(1, point(1_000_000.0, 0.0), now + Duration::from_millis(1)),
            1,
        );
        assert!(core.begin(begin(2, now, &drops)).is_empty());
        let mut returned = None;
        for index in 1..=MAX_SAMPLES {
            returned = core.motion(
                2,
                point(1000.0, -500.0),
                now + Duration::from_nanos(index as u64),
            );
            if returned.is_some() {
                break;
            }
        }
        cancelled(returned, 2);
        assert!(core.sequence().is_none());
        assert!(core
            .end(2, point(1000.0, -500.0), now + Duration::from_millis(1), ())
            .is_none());
        assert_eq!(*drops.borrow(), [1, 2]);
    }
}
