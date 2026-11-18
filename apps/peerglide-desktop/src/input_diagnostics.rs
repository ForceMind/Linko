use peerglide_platform::input_diagnostics::{
    is_held_line, is_summary, HeldCause, InputDiagnostics, Phase, Snapshot, PEER_PREFIX,
};
use peerglide_platform::now_ms;
use peerglide_protocol::{peer_diagnostics, Message, MAX_FRAME};
use peerglide_transport as transport;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::{collections::VecDeque, io::Write, sync::Arc, time::Duration};
use tokio::{sync::oneshot, time::Instant};

const MAX_PENDING_HEARTBEATS: usize = 8;
/// Held-state lines written for gate blocks and busy denials per session.
pub(super) const MAX_HELD_EVENTS: u8 = 5;
const REPORT_QUEUE: usize = 8;
const SAMPLE_INTERVAL_MS: u64 = 1000;
/// Frames one session sends: one per second over the 120 s window.
const MAX_PEER_FRAMES: u32 = 150;
/// Frames one session accepts; later frames are ignored.
const MAX_PEER_ACCEPTED: u32 = 300;
const PEER_MIN_GAP_MS: u64 = 250;

enum Report {
    Summary(Snapshot),
    Line(String),
}

/// Non-blocking handle to the diagnostic writer thread.
#[derive(Clone)]
pub(super) struct DiagnosticOutput(std::sync::mpsc::SyncSender<Report>);

impl DiagnosticOutput {
    /// A stalled stderr reader drops the line instead of delaying input.
    pub fn line(&self, line: String) {
        let _ = self.0.try_send(Report::Line(line));
    }
}

/// Session-owned diagnostic lines. Reads atomics written by the capture
/// callback; never runs inside a callback.
pub(super) struct SessionDiagnostics<'a> {
    counters: Option<&'a Arc<InputDiagnostics>>,
    output: Option<DiagnosticOutput>,
    held_events: u8,
    next_sample_ms: u64,
    peer: Option<PeerOutbox>,
}

/// Present only when both sides offered the ADR-0019 exchange.
struct PeerOutbox {
    sent: u32,
    events: VecDeque<String>,
}

impl PeerOutbox {
    fn new() -> Self {
        Self {
            sent: 0,
            events: VecDeque::with_capacity(usize::from(MAX_HELD_EVENTS)),
        }
    }
}

/// Receiver side of the exchange, owned by the stream reader task.
pub(super) struct PeerInbox {
    output: DiagnosticOutput,
    accepted: u32,
    last_ms: Option<u64>,
}

impl PeerInbox {
    /// Lines to show for one report. Any invalid line rejects the report;
    /// early or excess reports are ignored.
    fn lines(&mut self, report: peer_diagnostics::Report, now: u64) -> Vec<String> {
        let early = self.last_ms.is_some_and(|last| now < last.saturating_add(PEER_MIN_GAP_MS));
        if early || self.accepted >= MAX_PEER_ACCEPTED {
            return Vec::new();
        }
        let valid = report.summary.as_deref().is_none_or(is_summary)
            && report.held.as_deref().is_none_or(|line| is_held_line(line, true))
            && report.event.as_deref().is_none_or(|line| is_held_line(line, false));
        if !valid {
            return Vec::new();
        }
        self.accepted += 1;
        self.last_ms = Some(now);
        let fields = [report.summary, report.held, report.event];
        let mut lines = Vec::new();
        for line in fields.into_iter().flatten() {
            lines.push(format!("{PEER_PREFIX}{line}"));
        }
        lines
    }

    fn accept(&mut self, report: peer_diagnostics::Report, now: u64) {
        for line in self.lines(report, now) {
            self.output.line(line);
        }
    }
}

/// Reads the v5 stream when both sides offered the exchange. A frame that
/// is neither a v5 message nor a valid report fails exactly as before.
pub(super) async fn receive_with_peer(
    rx: &mut quinn::RecvStream,
    inbox: &mut PeerInbox,
) -> anyhow::Result<Message> {
    let mut buffer = [0; MAX_FRAME];
    loop {
        let frame = transport::receive_frame(rx, &mut buffer).await?;
        let message = transport::decode_message(frame);
        if message.is_ok() {
            return message;
        }
        let Some(report) = peer_diagnostics::decode_report(frame) else {
            return message;
        };
        inbox.accept(report, now_ms());
    }
}

impl<'a> SessionDiagnostics<'a> {
    pub fn new(
        counters: Option<&'a Arc<InputDiagnostics>>,
        output: Option<DiagnosticOutput>,
        peer: bool,
    ) -> Self {
        let enabled = peer && counters.is_some() && output.is_some();
        let peer = enabled.then(PeerOutbox::new);
        Self {
            counters,
            output,
            held_events: 0,
            next_sample_ms: 0,
            peer,
        }
    }

    /// Receiver state for the stream reader, when the exchange is on.
    pub fn peer_inbox(&self) -> Option<PeerInbox> {
        let output = self.peer.as_ref().and(self.output.clone())?;
        Some(PeerInbox {
            output,
            accepted: 0,
            last_ms: None,
        })
    }

    /// Writes what the gate counts as held when a crossing or a peer
    /// request is refused. Categories and counts only, never key codes.
    pub fn held_event(&mut self, cause: HeldCause, gate: u64, pending: bool, outgoing: bool) {
        let line = self.held_event_line(cause, gate, pending, outgoing);
        if let (Some(line), Some(output)) = (line, &self.output) {
            if let Some(peer) = self.peer.as_mut() {
                peer.events.push_back(line.clone());
            }
            output.line(line);
        }
    }

    /// Called from the session's 200 ms tick, outside callbacks. Writes the
    /// local held sample once per second while the window is active and
    /// returns the frame for the peer when the exchange is on.
    pub fn tick(&mut self, now: u64, gate: u64, pending: bool, outgoing: bool) -> Option<Vec<u8>> {
        let c = self.counters?;
        if now < self.next_sample_ms || !c.active() {
            return None;
        }
        self.next_sample_ms = now.saturating_add(SAMPLE_INTERVAL_MS);
        let held = c.held_report(HeldCause::Sample, gate, pending, outgoing);
        let held = held.to_string();
        if let Some(output) = &self.output {
            output.line(held.clone());
        }
        let peer = self.peer.as_mut()?;
        if peer.sent >= MAX_PEER_FRAMES {
            return None;
        }
        let summary = c.snapshot(Phase::Sample).to_string();
        let report = peer_diagnostics::Report {
            summary: is_summary(&summary).then_some(summary),
            held: is_held_line(&held, true).then_some(held),
            event: peer.events.pop_front(),
        };
        let frame = peer_diagnostics::encode_report(&report)?;
        peer.sent += 1;
        Some(frame)
    }

    fn held_event_line(
        &mut self,
        cause: HeldCause,
        gate: u64,
        pending: bool,
        outgoing: bool,
    ) -> Option<String> {
        let counters = self.counters?;
        if self.held_events >= MAX_HELD_EVENTS {
            return None;
        }
        self.held_events += 1;
        let report = counters.held_report(cause, gate, pending, outgoing);
        Some(report.to_string())
    }
}

/// Replies use the ordered v5 stream and carry their lease, so match them in
/// send order. If the queue overflows, stop sampling that lease.
pub(super) struct HeartbeatTiming {
    lease: u64,
    sent: VecDeque<Instant>,
    overflowed: bool,
}

impl HeartbeatTiming {
    pub fn new() -> Self {
        Self {
            lease: 0,
            sent: VecDeque::with_capacity(MAX_PENDING_HEARTBEATS),
            overflowed: false,
        }
    }

    pub fn sent(&mut self, lease: u64, time: Instant) {
        if self.lease != lease {
            self.lease = lease;
            self.sent.clear();
            self.overflowed = false;
        }
        if self.sent.len() == MAX_PENDING_HEARTBEATS {
            self.overflowed = true;
        }
        if !self.overflowed {
            self.sent.push_back(time);
        }
    }

    pub fn acknowledged(&mut self, lease: u64, time: Instant) -> Option<Duration> {
        if self.lease != lease || self.overflowed {
            return None;
        }
        self.sent
            .pop_front()
            .map(|sent| time.saturating_duration_since(sent))
    }
}

pub(super) struct DiagnosticWindow {
    pub counters: Arc<InputDiagnostics>,
    output: DiagnosticOutput,
    finish: oneshot::Sender<()>,
    written: oneshot::Receiver<()>,
}

impl DiagnosticWindow {
    pub fn start() -> std::io::Result<Self> {
        let counters = Arc::new(InputDiagnostics::default());
        let observed = Arc::clone(&counters);
        let (reports, output) = std::sync::mpsc::sync_channel::<Report>(REPORT_QUEUE);
        let lines = DiagnosticOutput(reports.clone());
        let (finish, mut finished) = oneshot::channel();
        let (drained, written) = oneshot::channel();
        // The writer must not hold Rust's global stderr lock while a pipe is
        // backpressured. Recovery can still use the ordinary diagnostics path.
        let stderr = std::io::stderr();
        #[cfg(unix)]
        let mut stream = std::fs::File::from(stderr.as_fd().try_clone_to_owned()?);
        #[cfg(windows)]
        let mut stream = std::fs::File::from(stderr.as_handle().try_clone_to_owned()?);
        std::thread::Builder::new()
            .name("input-diagnostics".into())
            .spawn(move || {
                for report in output {
                    let text = match report {
                        Report::Summary(summary) => summary.to_string(),
                        Report::Line(line) => line,
                    };
                    if stream.write_all(format!("{text}\n").as_bytes()).is_err() {
                        break;
                    }
                }
                let _ = drained.send(());
            })?;
        let started = Instant::now();
        tokio::spawn(async move {
            let interval = Duration::from_secs(1);
            let mut tick = tokio::time::interval_at(started + interval, interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let deadline = tokio::time::sleep_until(started + Duration::from_secs(120));
            tokio::pin!(deadline);
            let phase = loop {
                tokio::select! {
                    biased;
                    _ = &mut finished => break Phase::End,
                    _ = &mut deadline => break Phase::Expired,
                    _ = tick.tick() => {
                        let sample = observed.snapshot(Phase::Sample);
                        let _ = reports.try_send(Report::Summary(sample));
                    }
                }
            };
            observed.stop();
            let _ = reports.try_send(Report::Summary(observed.snapshot(phase)));
        });
        Ok(Self {
            counters,
            output: lines,
            finish,
            written,
        })
    }

    pub fn output(&self) -> DiagnosticOutput {
        self.output.clone()
    }

    pub async fn finish(self) {
        let Self {
            finish,
            written,
            output,
            ..
        } = self;
        // The writer ends once every line handle is gone.
        drop(output);
        let _ = finish.send(());
        // Native cleanup has already finished. A blocked output reader can
        // discard diagnostics but cannot delay recovery or keep the worker alive.
        let _ = tokio::time::timeout(Duration::from_millis(200), written).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> (DiagnosticOutput, std::sync::mpsc::Receiver<Report>) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(REPORT_QUEUE);
        (DiagnosticOutput(sender), receiver)
    }

    fn written(receiver: &std::sync::mpsc::Receiver<Report>) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(report) = receiver.try_recv() {
            if let Report::Line(line) = report {
                lines.push(line);
            }
        }
        lines
    }

    #[test]
    fn held_events_are_bounded_per_session_and_need_a_diagnostic_session() {
        let counters = Arc::new(InputDiagnostics::default());
        let mut s = SessionDiagnostics::new(Some(&counters), None, false);
        let (gate, busy) = (HeldCause::BlockGate, HeldCause::DenyBusy);
        let mut lines = Vec::new();
        for _ in 0..4 {
            lines.extend(s.held_event_line(gate, 1, false, false));
            lines.extend(s.held_event_line(busy, 1, true, false));
        }
        assert_eq!(lines.len(), usize::from(MAX_HELD_EVENTS));
        assert!(lines.iter().all(|line| is_held_line(line, false)));
        assert!(lines[0].starts_with("KVM-HELD/1 blk_gate "));
        assert!(lines[1].starts_with("KVM-HELD/1 deny_busy "));
        let mut plain = SessionDiagnostics::new(None, None, true);
        assert!(plain.held_event_line(gate, 1, false, false).is_none());
        assert!(plain.peer_inbox().is_none());
        assert!(plain.tick(0, 0, false, false).is_none());
    }

    #[test]
    fn peer_frames_need_both_offers_and_carry_only_validated_lines() {
        let counters = Arc::new(InputDiagnostics::default());
        let (lines, local) = output();
        let mut alone = SessionDiagnostics::new(Some(&counters), Some(lines), false);
        assert!(alone.peer_inbox().is_none());
        assert!(alone.tick(0, 0, false, false).is_none());
        let samples = written(&local);
        assert_eq!(samples.len(), 1);
        assert!(is_held_line(&samples[0], true), "{}", samples[0]);

        let (lines, local) = output();
        let mut both = SessionDiagnostics::new(Some(&counters), Some(lines), true);
        assert!(both.peer_inbox().is_some());
        both.held_event(HeldCause::BlockGate, 2, false, false);
        let frame = both.tick(0, 2, false, false).unwrap();
        assert!(both.tick(999, 2, false, false).is_none());
        assert_eq!(written(&local).len(), 2);
        let report = peer_diagnostics::decode_report(&frame).unwrap();
        assert!(report.summary.as_deref().is_some_and(is_summary));
        let (held, event) = (report.held.unwrap(), report.event.unwrap());
        assert!(is_held_line(&held, true), "{held}");
        assert!(is_held_line(&event, false), "{event}");
        let second = both.tick(1000, 0, false, false).unwrap();
        let second = peer_diagnostics::decode_report(&second).unwrap();
        assert_eq!(second.event, None, "each event is sent once");

        counters.stop();
        assert!(both.tick(5000, 0, false, false).is_none());
    }

    #[test]
    fn peer_reports_are_rate_limited_and_rejected_whole_when_invalid() {
        let counters = InputDiagnostics::default();
        let held = counters.held_report(HeldCause::Sample, 0, false, false);
        let event = counters.held_report(HeldCause::DenyBusy, 1, false, true);
        let report = peer_diagnostics::Report {
            summary: Some(counters.snapshot(Phase::Sample).to_string()),
            held: Some(held.to_string()),
            event: Some(event.to_string()),
        };
        let (lines, _local) = output();
        let mut inbox = PeerInbox {
            output: lines,
            accepted: 0,
            last_ms: None,
        };
        let shown = inbox.lines(report.clone(), 1000);
        assert_eq!(shown.len(), 3);
        assert!(shown.iter().all(|line| line.starts_with(PEER_PREFIX)));
        assert!(inbox.lines(report.clone(), 1100).is_empty(), "too early");
        assert_eq!(inbox.lines(report.clone(), 1250).len(), 3);
        let mut forged = report.clone();
        forged.held = Some("KVM-HELD/1 sample key=4".into());
        assert!(inbox.lines(forged, 5000).is_empty());
        let mut swapped = report.clone();
        swapped.event = report.held.clone();
        assert!(inbox.lines(swapped, 6000).is_empty());
        inbox.accepted = MAX_PEER_ACCEPTED;
        assert!(inbox.lines(report, 9000).is_empty());
    }

    #[tokio::test]
    async fn stream_reader_shows_reports_and_keeps_v5_failure_classes() -> anyhow::Result<()> {
        let a = transport::Identity::generate()?;
        let b = transport::Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let address = eb.local_addr()?;
        let counters = InputDiagnostics::default();
        let held = counters.held_report(HeldCause::Sample, 0, false, false);
        let report = peer_diagnostics::Report {
            held: Some(held.to_string()),
            ..Default::default()
        };
        let frame = peer_diagnostics::encode_report(&report).unwrap();
        let (lines, local) = output();
        let mut inbox = PeerInbox {
            output: lines,
            accepted: 0,
            last_ms: None,
        };
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (_tx, mut rx) = c.accept_bi().await?;
            let first = receive_with_peer(&mut rx, &mut inbox).await?;
            let second = receive_with_peer(&mut rx, &mut inbox).await;
            Ok::<_, anyhow::Error>((c, first, second))
        };
        let client = async {
            let c = ea.connect(address, "peerglide.local")?.await?;
            let (mut tx, _rx) = c.open_bi().await?;
            transport::send_frame(&mut tx, &frame).await?;
            transport::send(&mut tx, &Message::Ping { lease: 0 }).await?;
            transport::send_frame(&mut tx, br#"{"Unknown":{}}"#).await?;
            Ok::<_, anyhow::Error>((c, tx))
        };
        let (server, _client) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        let (_connection, first, second) = server;
        assert!(matches!(first, Message::Ping { lease: 0 }));
        let error = second.err().unwrap();
        assert!(error.is::<transport::InvalidMessageFrame>());
        let shown = written(&local);
        assert_eq!(shown.len(), 1);
        assert!(shown[0].starts_with(PEER_PREFIX), "{}", shown[0]);
        Ok(())
    }

    #[test]
    fn heartbeat_timings_match_in_order_and_discard_retired_leases() {
        let mut timing = HeartbeatTiming::new();
        let start = Instant::now();
        let after = |ms| start + Duration::from_millis(ms);
        timing.sent(1, start);
        timing.sent(1, after(10));
        assert_eq!(timing.acknowledged(2, after(11)), None);
        assert_eq!(
            timing.acknowledged(1, after(20)),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            timing.acknowledged(1, after(25)),
            Some(Duration::from_millis(15))
        );
        assert_eq!(timing.acknowledged(1, after(25)), None);

        for _ in 0..=MAX_PENDING_HEARTBEATS {
            timing.sent(1, after(30));
        }
        assert_eq!(timing.acknowledged(1, after(40)), None);
        timing.sent(2, after(50));
        assert_eq!(timing.acknowledged(1, after(60)), None);
        assert_eq!(
            timing.acknowledged(2, after(65)),
            Some(Duration::from_millis(15))
        );
    }
}
