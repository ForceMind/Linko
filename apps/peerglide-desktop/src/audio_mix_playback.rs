//! One local playback owner for an explicitly authorized set of audio sources.
use anyhow::{Context, Result};
use peerglide_core::{
    audio_mix::{Mix, Source, MAX_SOURCES},
    audio_playout::SAMPLES,
};
use peerglide_transport::audio::PlaybackListener;
use std::{future::Future, time::Duration};
use tokio::{sync::mpsc, task::JoinSet, time::Instant};

const PERIOD: Duration = Duration::from_millis(5);

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
    let mut workers: JoinSet<Source> = JoinSet::new();
    let (frames, mut received) = mpsc::channel(MAX_SOURCES * 4);
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
            result = &mut stop => return Ok(result?),
            _ = tokio::time::sleep_until(deadline) => return Ok(()),
            closed = workers.join_next(), if !workers.is_empty() => {
                let source = closed.context("audio source disappeared")??;
                mix.stop(source);
                if playing && workers.is_empty() {
                    playing = false;
                    submit(None)?;
                }
            }
            _ = tick.tick(), if !workers.is_empty() => {
                playing |= mix.render(&mut output);
                if playing {
                    submit(Some(&output))?;
                }
            }
            item = received.recv() => {
                let (source, frame) = item.context("audio source queue closed")?;
                mix.push(source, frame);
            }
            accepted = &mut incoming => {
                if let Ok((slot, session)) = accepted {
                    if let Some(source) = mix.start(slot) {
                        let frames = frames.clone();
                        workers.spawn(async move {
                            while let Ok(frame) = session.receive().await {
                                if let Err(mpsc::error::TrySendError::Closed(_)) = frames.try_send((source, frame)) {
                                    break;
                                }
                            }
                            source
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
    use peerglide_core::audio_playout::Frame;
    use peerglide_transport::{audio, Identity};
    use tokio::sync::oneshot;

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
