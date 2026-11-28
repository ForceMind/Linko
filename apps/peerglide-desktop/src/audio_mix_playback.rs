//! One local playback owner for an explicitly authorized set of audio sources.
use anyhow::{Context, Result};
use peerglide_core::{
    audio_mix::{Mix, Source, MAX_SOURCES},
    audio_playout::{Frame, SAMPLES},
};
use peerglide_transport::audio::PlaybackListener;
use std::{future::Future, time::Duration};
use tokio::{sync::mpsc, task::JoinSet, time::Instant};

const PERIOD: Duration = Duration::from_millis(5);
const SOURCE_QUEUE: usize = 4;

#[cfg(any(windows, target_os = "macos"))]
pub(super) async fn run(
    identity: &std::path::Path,
    certificates: &[std::path::PathBuf],
    bind: std::net::SocketAddr,
    device: Option<&str>,
    volume: u8,
    duration: Duration,
) -> Result<()> {
    use peerglide_transport::{self as transport, Identity};
    anyhow::ensure!(
        (1..=MAX_SOURCES).contains(&certificates.len()),
        "expected one to four audio sources"
    );
    let identity = Identity::load(identity)?;
    let peers = certificates
        .iter()
        .map(|path| transport::read_certificate(path))
        .collect::<Result<Vec<_>>>()?;
    let listener = PlaybackListener::bind(bind, &identity, peers)?;
    println!(
        "Waiting for authenticated audio sources on {}.",
        listener.local_addr()?
    );
    let mut player = None;
    let result = drive(&listener, duration, tokio::signal::ctrl_c(), |samples| {
        if let Some(samples) = samples {
            if player.is_none() {
                player = Some(super::audio_playback::open_player(device)?);
            }
            if let Some(player) = player.as_mut() {
                player.submit(samples, volume)?;
            }
        } else if let Some(mut player) = player.take() {
            player.stop().context("local audio output cleanup failed")?;
        }
        Ok(())
    })
    .await;
    listener.close();
    if let Some(mut player) = player {
        player.stop().context("local audio output cleanup failed")?;
    }
    result
}

struct BufferedSource {
    source: Source,
    frames: mpsc::Receiver<Frame>,
}

impl BufferedSource {
    fn drain(&mut self, mix: &mut Mix) {
        for _ in 0..SOURCE_QUEUE {
            let Ok(frame) = self.frames.try_recv() else {
                break;
            };
            mix.push(self.source, frame);
        }
    }
}

async fn drive<F>(
    listener: &PlaybackListener,
    duration: Duration,
    stop: F,
    mut submit: impl FnMut(Option<&[i16; SAMPLES]>) -> Result<()>,
) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let mut mix = Mix::default();
    let mut workers: JoinSet<(usize, Source)> = JoinSet::new();
    let mut sources: [Option<BufferedSource>; MAX_SOURCES] = Default::default();
    let mut tick = tokio::time::interval(PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = Instant::now() + duration;
    let incoming = listener.accept(duration);
    let mut output = [0; SAMPLES];
    let mut playing = false;
    tokio::pin!(incoming, stop);
    loop {
        tokio::select! {
            biased;
            result = &mut stop => return result.map_err(Into::into),
            _ = tokio::time::sleep_until(deadline) => return Ok(()),
            closed = workers.join_next(), if !workers.is_empty() => {
                let (slot, source) = closed.context("audio source disappeared")??;
                sources[slot] = None;
                mix.stop(source);
                if playing && workers.is_empty() {
                    playing = false;
                    submit(None)?;
                }
            }
            _ = tick.tick(), if !workers.is_empty() => {
                for source in sources.iter_mut().flatten() {
                    source.drain(&mut mix);
                }
                playing |= mix.render(&mut output);
                if playing {
                    submit(Some(&output))?;
                }
            }
            accepted = &mut incoming => {
                if let Ok((slot, session)) = accepted {
                    if let Some(source) = mix.start(slot) {
                        let (send, frames) = mpsc::channel(SOURCE_QUEUE);
                        sources[slot] = Some(BufferedSource { source, frames });
                        workers.spawn(async move {
                            while let Ok(frame) = session.receive().await {
                                if let Err(mpsc::error::TrySendError::Closed(_)) = send.try_send(frame) {
                                    break;
                                }
                            }
                            (slot, source)
                        });
                    }
                }
                incoming.set(listener.accept(deadline.saturating_duration_since(Instant::now())));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_transport::{audio, Identity};
    use tokio::sync::oneshot;

    #[test]
    fn full_source_queue_preserves_space_for_the_other_source() {
        let mut mix = Mix::default();
        let (left_send, left_frames) = mpsc::channel(SOURCE_QUEUE);
        let (right_send, right_frames) = mpsc::channel(SOURCE_QUEUE);
        let mut left = BufferedSource {
            source: mix.start(0).unwrap(),
            frames: left_frames,
        };
        let mut right = BufferedSource {
            source: mix.start(1).unwrap(),
            frames: right_frames,
        };
        for sequence in 1..=SOURCE_QUEUE as u64 {
            assert!(left_send
                .try_send(Frame {
                    sequence,
                    samples: [7; SAMPLES]
                })
                .is_ok());
        }
        assert!(left_send
            .try_send(Frame {
                sequence: 5,
                samples: [9; SAMPLES]
            })
            .is_err());
        for sequence in 1..=3 {
            assert!(right_send
                .try_send(Frame {
                    sequence,
                    samples: [5; SAMPLES]
                })
                .is_ok());
        }
        left.drain(&mut mix);
        right.drain(&mut mix);
        let mut output = [0; SAMPLES];
        assert!(mix.render(&mut output));
        assert_eq!(output, [12; SAMPLES]);
    }

    #[tokio::test]
    async fn mix_driver_delivers_authenticated_audio_and_retires_the_last_output() -> Result<()> {
        let sink = Identity::generate()?;
        let source = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let listener = PlaybackListener::bind(bind, &sink, vec![source.certificate.clone()])?;
        let address = listener.local_addr()?;
        let duration = Duration::from_secs(5);
        let (played, heard) = oneshot::channel();
        let (stopped, stop) = oneshot::channel();
        let mut played = Some(played);
        let mut stopped = Some(stopped);
        let receiver = drive(
            &listener,
            duration,
            async {
                stop.await
                    .map_err(|_| std::io::Error::other("audio fixture ended"))
            },
            |samples| {
                if let Some(samples) = samples {
                    if *samples == [33; SAMPLES] {
                        if let Some(played) = played.take() {
                            let _ = played.send(());
                        }
                    }
                } else if let Some(stopped) = stopped.take() {
                    let _ = stopped.send(());
                }
                Ok(())
            },
        );
        let sender = async {
            let endpoint = audio::endpoint(bind, &source, &sink.certificate)?;
            let connection = endpoint.connect(address, "peerglide.local")?.await?;
            let mut session = audio::open(
                &connection,
                &sink.certificate,
                audio::Direction::Send,
                duration,
            )
            .await?;
            assert!(session.send(&Frame {
                sequence: 1,
                samples: [33; SAMPLES]
            })?);
            heard.await?;
            session.stop();
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(duration, async { tokio::try_join!(receiver, sender) }).await??;
        assert!(played.is_none() && stopped.is_none());
        Ok(())
    }
}
