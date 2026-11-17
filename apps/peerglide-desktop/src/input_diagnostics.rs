use peerglide_platform::input_diagnostics::{InputDiagnostics, Phase, Snapshot};
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::{collections::VecDeque, io::Write, sync::Arc, time::Duration};
use tokio::{sync::oneshot, time::Instant};

const MAX_PENDING_HEARTBEATS: usize = 8;

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
    finish: oneshot::Sender<()>,
    written: oneshot::Receiver<()>,
}

impl DiagnosticWindow {
    pub fn start() -> std::io::Result<Self> {
        let counters = Arc::new(InputDiagnostics::default());
        let observed = Arc::clone(&counters);
        let (reports, output) = std::sync::mpsc::sync_channel::<Snapshot>(2);
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
                    if stream.write_all(format!("{report}\n").as_bytes()).is_err() {
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
                        let _ = reports.try_send(observed.snapshot(Phase::Sample));
                    }
                }
            };
            observed.stop();
            let _ = reports.try_send(observed.snapshot(phase));
        });
        Ok(Self {
            counters,
            finish,
            written,
        })
    }

    pub async fn finish(self) {
        let _ = self.finish.send(());
        // Native cleanup has already finished. A blocked output reader can
        // discard diagnostics but cannot delay recovery or keep the worker alive.
        let _ = tokio::time::timeout(Duration::from_millis(200), self.written).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
