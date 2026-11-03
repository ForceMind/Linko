//! G03 pure ownership model for the existing native input adapter to host later.
//! No hook, native replay, network forwarding or action execution occurs here.
use peerglide_actions::Context;
use peerglide_gestures::{Match, Point, Recognizer, Rejection, Sample, Stroke};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    PhysicalLocal,
    RemoteInjected,
    ActionInjected,
    RecoveryInjected,
}
#[derive(Clone, Copy, Debug)]
pub enum Event {
    RightDown(Point),
    Move(Point),
    RightUp(Point),
    OtherPhysical,
}
#[derive(Clone, Copy, Debug)]
pub struct Environment {
    /// Explicit G1 consent + valid native permissions + no excluded app/modifiers.
    /// This is unrelated to pairing/input grants; the local mode needs no peer.
    pub eligible: bool,
    pub remote_target: bool,
    pub context: Context,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    BypassHeld,
    Pending,
    Tracking,
    CancelledHeld,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    PassLocal,
    SuppressRightButton,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Context,
    Disabled,
    Clock,
    LongHold,
    Emergency,
    Recognition(Rejection),
}
#[derive(Debug)]
pub enum Intent {
    BalancedRightClick(Context),
    Gesture { context: Context, matched: Match },
}
#[derive(Debug)]
pub struct Ticket {
    context: Context,
    deadline: Duration,
    issued: Duration,
    intent: Intent,
}
impl Ticket {
    /// Pure context/deadline check, NOT OS focus authentication or action consent.
    /// Native dispatch must recheck permissions and report submission honestly.
    pub fn check(self, current: Environment, now: Duration) -> Result<Intent, CancelReason> {
        if !current.eligible || current.remote_target {
            return Err(CancelReason::Disabled);
        }
        if current.context != self.context {
            return Err(CancelReason::Context);
        }
        if now < self.issued || now >= self.deadline {
            return Err(CancelReason::Clock);
        }
        Ok(self.intent)
    }
}
#[derive(Debug)]
pub enum Completion {
    Ready(Box<Ticket>),
    Cancelled(CancelReason),
}
#[derive(Debug)]
pub struct Effect {
    pub delivery: Delivery,
    pub block_edges: bool,
    /// Caller must require interior movement before an edge switch can rearm.
    pub rearm_edge: bool,
    pub completion: Option<Completion>,
}
struct Candidate<'a> {
    stroke: Stroke<'a>,
    context: Context,
    start: Duration,
    last: Duration,
    deadline: Duration,
    first: Point,
    tracking: bool,
}
enum State<'a> {
    Idle,
    Bypass,
    Candidate(Box<Candidate<'a>>),
    Cancelled,
}
pub struct GestureBroker<'a> {
    recognizer: &'a Recognizer,
    state: State<'a>,
}
impl<'a> GestureBroker<'a> {
    pub fn new(recognizer: &'a Recognizer) -> Self {
        Self {
            recognizer,
            state: State::Idle,
        }
    }
    pub fn phase(&self) -> Phase {
        match &self.state {
            State::Idle => Phase::Idle,
            State::Bypass => Phase::BypassHeld,
            State::Cancelled => Phase::CancelledHeld,
            State::Candidate(c) if c.tracking => Phase::Tracking,
            State::Candidate(_) => Phase::Pending,
        }
    }
    pub fn cancel(&mut self) -> Effect {
        if matches!(self.state, State::Candidate(_)) {
            self.state = State::Cancelled;
            effect(
                Delivery::PassLocal,
                false,
                true,
                Some(Completion::Cancelled(CancelReason::Emergency)),
            )
        } else {
            effect(Delivery::PassLocal, false, false, None)
        }
    }
    pub fn handle(
        &mut self,
        event: Event,
        origin: Origin,
        env: Environment,
        now: Duration,
    ) -> Effect {
        if origin != Origin::PhysicalLocal {
            return effect(Delivery::PassLocal, false, false, None);
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        match state {
            State::Idle => match event {
                Event::RightDown(point) if env.eligible && !env.remote_target => {
                    let Some(deadline) = now.checked_add(self.recognizer.config().max_duration)
                    else {
                        self.state = State::Bypass;
                        return effect(Delivery::PassLocal, false, false, None);
                    };
                    let mut stroke = self.recognizer.stroke();
                    if stroke
                        .push(Sample {
                            point,
                            elapsed: Duration::ZERO,
                        })
                        .is_err()
                    {
                        self.state = State::Bypass;
                        return effect(Delivery::PassLocal, false, false, None);
                    }
                    self.state = State::Candidate(Box::new(Candidate {
                        stroke,
                        context: env.context,
                        start: now,
                        last: now,
                        deadline,
                        first: point,
                        tracking: false,
                    }));
                    effect(Delivery::SuppressRightButton, true, false, None)
                }
                Event::RightDown(_) => {
                    self.state = State::Bypass;
                    effect(Delivery::PassLocal, false, false, None)
                }
                _ => effect(Delivery::PassLocal, false, false, None),
            },
            State::Bypass => {
                if !matches!(event, Event::RightUp(_)) {
                    self.state = State::Bypass;
                }
                effect(Delivery::PassLocal, false, false, None)
            }
            State::Cancelled => {
                if !matches!(event, Event::RightUp(_)) {
                    self.state = State::Cancelled;
                }
                effect(
                    if matches!(event, Event::RightDown(_) | Event::RightUp(_)) {
                        Delivery::SuppressRightButton
                    } else {
                        Delivery::PassLocal
                    },
                    false,
                    false,
                    None,
                )
            }
            State::Candidate(mut c) => {
                let up = matches!(event, Event::RightUp(_));
                let failure = if now < c.last {
                    Some(CancelReason::Clock)
                } else if !env.eligible || env.remote_target {
                    Some(CancelReason::Disabled)
                } else if env.context != c.context {
                    Some(CancelReason::Context)
                } else if now >= c.deadline {
                    Some(CancelReason::Recognition(Rejection::Duration))
                } else if matches!(event, Event::OtherPhysical) {
                    Some(CancelReason::Disabled)
                } else {
                    None
                };
                let delivery = if matches!(event, Event::RightDown(_) | Event::RightUp(_)) {
                    Delivery::SuppressRightButton
                } else {
                    Delivery::PassLocal
                };
                if let Some(reason) = failure {
                    self.state = if up { State::Idle } else { State::Cancelled };
                    return effect(delivery, false, true, Some(Completion::Cancelled(reason)));
                }
                c.last = now;
                if let Event::Move(point) | Event::RightUp(point) = event {
                    if let Err(e) = c.stroke.push(Sample {
                        point,
                        elapsed: now - c.start,
                    }) {
                        self.state = if up { State::Idle } else { State::Cancelled };
                        return effect(
                            delivery,
                            false,
                            true,
                            Some(Completion::Cancelled(CancelReason::Recognition(e))),
                        );
                    }
                    if (point.x - c.first.x).hypot(point.y - c.first.y) >= 12. {
                        c.tracking = true;
                    }
                }
                if !up {
                    self.state = State::Candidate(c);
                    return effect(delivery, true, false, None);
                }
                let result = if !c.tracking {
                    if now - c.start > Duration::from_millis(400) {
                        Err(CancelReason::LongHold)
                    } else {
                        Ok(Intent::BalancedRightClick(c.context))
                    }
                } else {
                    c.stroke
                        .finish(now - c.start)
                        .map(|matched| Intent::Gesture {
                            context: c.context,
                            matched,
                        })
                        .map_err(CancelReason::Recognition)
                };
                let completion = Some(match result {
                    Ok(intent) => Completion::Ready(Box::new(Ticket {
                        context: c.context,
                        deadline: c.deadline,
                        issued: now,
                        intent,
                    })),
                    Err(reason) => Completion::Cancelled(reason),
                });
                effect(delivery, false, true, completion)
            }
        }
    }
}
fn effect(
    delivery: Delivery,
    block_edges: bool,
    rearm_edge: bool,
    completion: Option<Completion>,
) -> Effect {
    Effect {
        delivery,
        block_edges,
        rearm_edge,
        completion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_gestures::{starter_templates, Config};
    fn recognizer() -> Recognizer {
        Recognizer::new(Config::default(), starter_templates()).unwrap()
    }
    fn env() -> Environment {
        Environment {
            eligible: true,
            remote_target: false,
            context: Context {
                device: [1; 32],
                target: [2; 32],
                focus_generation: 1,
                configuration_generation: 1,
                trigger_sequence: 1,
            },
        }
    }
    fn p(x: f64) -> Point {
        Point::new(x, 0.)
    }
    fn send(b: &mut GestureBroker, e: Event, t: u64) -> Effect {
        b.handle(e, Origin::PhysicalLocal, env(), Duration::from_millis(t))
    }
    fn ticket(e: Effect) -> Ticket {
        match e.completion {
            Some(Completion::Ready(t)) => *t,
            _ => panic!("no ticket"),
        }
    }
    #[test]
    fn short_click_is_balanced_intent_once_not_a_raw_up() {
        let r = recognizer();
        let mut b = GestureBroker::new(&r);
        assert_eq!(
            send(&mut b, Event::RightDown(p(0.)), 0).delivery,
            Delivery::SuppressRightButton
        );
        let done = send(&mut b, Event::RightUp(p(1.)), 100);
        assert!(done.rearm_edge);
        assert!(matches!(
            ticket(done)
                .check(env(), Duration::from_millis(101))
                .unwrap(),
            Intent::BalancedRightClick(_)
        ));
        assert!(send(&mut b, Event::RightUp(p(1.)), 102)
            .completion
            .is_none());
    }
    #[test]
    fn disabling_mid_hold_cannot_steal_a_passed_pair_or_emit_orphan_up() {
        let r = recognizer();
        let mut b = GestureBroker::new(&r);
        let mut off = env();
        off.eligible = false;
        assert_eq!(
            b.handle(
                Event::RightDown(p(0.)),
                Origin::PhysicalLocal,
                off,
                Duration::ZERO
            )
            .delivery,
            Delivery::PassLocal
        );
        assert_eq!(
            send(&mut b, Event::RightUp(p(0.)), 100).delivery,
            Delivery::PassLocal
        );
        send(&mut b, Event::RightDown(p(0.)), 200);
        let cancelled = b.handle(
            Event::Move(p(30.)),
            Origin::PhysicalLocal,
            off,
            Duration::from_millis(210),
        );
        assert!(cancelled.rearm_edge);
        assert_eq!(b.phase(), Phase::CancelledHeld);
        assert_eq!(
            send(&mut b, Event::RightUp(p(30.)), 220).delivery,
            Delivery::SuppressRightButton
        );
        assert_eq!(b.phase(), Phase::Idle);
    }
    #[test]
    fn injected_events_and_remote_control_never_start_or_finish_a_gesture() {
        let r = recognizer();
        let mut b = GestureBroker::new(&r);
        for origin in [
            Origin::RemoteInjected,
            Origin::ActionInjected,
            Origin::RecoveryInjected,
        ] {
            assert_eq!(
                b.handle(Event::RightDown(p(0.)), origin, env(), Duration::ZERO)
                    .delivery,
                Delivery::PassLocal
            );
            assert_eq!(b.phase(), Phase::Idle);
        }
        send(&mut b, Event::RightDown(p(0.)), 0);
        b.handle(
            Event::RightUp(p(100.)),
            Origin::ActionInjected,
            env(),
            Duration::from_millis(100),
        );
        assert_eq!(b.phase(), Phase::Pending);
        b.cancel();
        send(&mut b, Event::RightUp(p(0.)), 101);
        let mut remote = env();
        remote.remote_target = true;
        assert_eq!(
            b.handle(
                Event::RightDown(p(0.)),
                Origin::PhysicalLocal,
                remote,
                Duration::from_millis(200)
            )
            .delivery,
            Delivery::PassLocal
        );
    }
    #[test]
    fn gesture_and_cancel_paths_require_edge_rearm_and_frozen_context() {
        let r = recognizer();
        let mut b = GestureBroker::new(&r);
        send(&mut b, Event::RightDown(p(0.)), 0);
        assert!(send(&mut b, Event::Move(p(60.)), 100).block_edges);
        assert_eq!(b.phase(), Phase::Tracking);
        let done = send(&mut b, Event::RightUp(p(100.)), 200);
        assert!(done.rearm_edge);
        let mut changed = env();
        changed.context.focus_generation += 1;
        assert_eq!(
            ticket(done)
                .check(changed, Duration::from_millis(201))
                .unwrap_err(),
            CancelReason::Context
        );
        send(&mut b, Event::RightDown(p(0.)), 300);
        let stop = b.cancel();
        assert!(stop.rearm_edge);
        assert_eq!(
            send(&mut b, Event::Move(p(99.)), 310).delivery,
            Delivery::PassLocal
        );
        assert_eq!(
            send(&mut b, Event::RightUp(p(99.)), 320).delivery,
            Delivery::SuppressRightButton
        );
    }
    #[test]
    fn unknown_long_clock_and_context_changes_cancel_without_click_fallback() {
        let r = recognizer();
        let mut unknown = GestureBroker::new(&r);
        send(&mut unknown, Event::RightDown(p(0.)), 0);
        send(&mut unknown, Event::Move(Point::new(60., 60.)), 100);
        assert!(matches!(
            send(&mut unknown, Event::RightUp(Point::new(100., 100.)), 200).completion,
            Some(Completion::Cancelled(CancelReason::Recognition(
                Rejection::NoMatch
            )))
        ));
        for time in [401, 2500] {
            let mut b = GestureBroker::new(&r);
            send(&mut b, Event::RightDown(p(0.)), 0);
            assert!(matches!(
                send(&mut b, Event::RightUp(p(0.)), time).completion,
                Some(Completion::Cancelled(_))
            ));
        }
        let mut b = GestureBroker::new(&r);
        send(&mut b, Event::RightDown(p(0.)), 100);
        assert!(matches!(
            send(&mut b, Event::Move(p(100.)), 99).completion,
            Some(Completion::Cancelled(CancelReason::Clock))
        ));
        send(&mut b, Event::RightUp(p(0.)), 101);
        send(&mut b, Event::RightDown(p(0.)), 200);
        let mut changed = env();
        changed.context.configuration_generation += 1;
        assert!(matches!(
            b.handle(
                Event::RightUp(p(0.)),
                Origin::PhysicalLocal,
                changed,
                Duration::from_millis(210)
            )
            .completion,
            Some(Completion::Cancelled(CancelReason::Context))
        ));
    }
    #[test]
    fn tickets_reject_revocation_clock_reversal_and_exact_expiry() {
        let r = recognizer();
        let prepare = || {
            let mut b = GestureBroker::new(&r);
            send(&mut b, Event::RightDown(p(0.)), 0);
            ticket(send(&mut b, Event::RightUp(p(1.)), 100))
        };
        assert_eq!(
            prepare()
                .check(env(), Duration::from_millis(99))
                .unwrap_err(),
            CancelReason::Clock
        );
        assert_eq!(
            prepare()
                .check(env(), Duration::from_millis(2500))
                .unwrap_err(),
            CancelReason::Clock
        );
        let mut denied = env();
        denied.eligible = false;
        assert_eq!(
            prepare()
                .check(denied, Duration::from_millis(101))
                .unwrap_err(),
            CancelReason::Disabled
        );
    }
    #[test]
    fn a_thousand_mode_transitions_keep_original_button_pair_ownership() {
        let r = recognizer();
        let mut b = GestureBroker::new(&r);
        for i in 0..1024 {
            let mut begin = env();
            begin.eligible = i % 2 == 0;
            let down = b.handle(
                Event::RightDown(p(0.)),
                Origin::PhysicalLocal,
                begin,
                Duration::from_secs(i),
            );
            let mut later = env();
            later.eligible = i % 3 == 0;
            b.handle(
                Event::Move(p(10.)),
                Origin::PhysicalLocal,
                later,
                Duration::from_secs(i) + Duration::from_millis(10),
            );
            if i % 5 == 0 {
                b.cancel();
            }
            let up = b.handle(
                Event::RightUp(p(10.)),
                Origin::PhysicalLocal,
                later,
                Duration::from_secs(i) + Duration::from_millis(20),
            );
            assert_eq!(down.delivery, up.delivery);
            assert_eq!(b.phase(), Phase::Idle);
        }
    }
}
