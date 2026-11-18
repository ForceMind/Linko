use peerglide_platform::input_diagnostics::{HeldCause, InputDiagnostics, Phase, Snapshot};
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
}

impl<'a> SessionDiagnostics<'a> {
    pub fn new(
        counters: Option<&'a Arc<InputDiagnostics>>,
        output: Option<DiagnosticOutput>,
    ) -> Self {
        Self {
            counters,
            output,
            held_events: 0,
        }
    }

    /// Writes what the gate counts as held when a crossing or a peer
    /// request is refused. Categories and counts only, never key codes.
    pub fn held_event(&mut self, cause: HeldCause, gate: u64, pending: bool, outgoing: bool) {
        if let Some(line) = self.held_event_line(cause, gate, pending, outgoing) {
            if let Some(output) = &self.output {
                output.line(line);
            }
        }
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
    use peerglide_platform::input_diagnostics::is_held_line;

    #[test]
    fn held_events_are_bounded_per_session_and_need_a_diagnostic_session() {
        let counters = Arc::new(InputDiagnostics::default());
        let mut s = SessionDiagnostics::new(Some(&counters), None);
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
        let mut plain = SessionDiagnostics::new(None, None);
        assert!(plain.held_event_line(gate, 1, false, false).is_none());
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
