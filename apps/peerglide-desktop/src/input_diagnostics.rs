use peerglide_platform::input_diagnostics::{InputDiagnostics, Phase, Snapshot};
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::{io::Write, sync::Arc, time::Duration};
use tokio::sync::oneshot;

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
        let started = tokio::time::Instant::now();
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
