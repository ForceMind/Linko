//! One explicitly authorized audio receiver with local output ownership.
use anyhow::{Context, Result};
use peerglide_core::audio_playout::{Buffer, Playout, SAMPLES};
#[cfg(any(windows, target_os = "macos", test))]
use peerglide_transport as transport;
use peerglide_transport::audio;
use std::{future::Future, time::Duration};

#[cfg(any(windows, target_os = "macos"))]
use std::{net::SocketAddr, path::Path};

#[cfg(any(windows, target_os = "macos"))]
const LISTEN_TIMEOUT: Duration = Duration::from_secs(60);
const PERIOD: Duration = Duration::from_millis(5);

#[cfg(target_os = "macos")]
use peerglide_platform::macos_audio as native;
#[cfg(windows)]
use peerglide_platform::windows_audio as native;

#[cfg(windows)]
pub(super) fn list_outputs() -> Result<()> {
    for device in native::output_devices() {
        print_device(&device.id.to_string(), &device.name);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn list_outputs() -> Result<()> {
    for device in native::output_devices()? {
        print_device(&device.uid, &device.name);
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn print_device(id: &str, name: &str) {
    let name: String = name.chars().filter(|character| !character.is_control()).collect();
    println!("{id}\t{name}");
}

#[cfg(any(windows, target_os = "macos"))]
pub(super) async fn run(
    identity: &Path,
    peer_certificate: &Path,
    bind: SocketAddr,
    device: Option<&str>,
    volume: u8,
    duration: Duration,
) -> Result<()> {
    let identity = transport::Identity::load(identity)?;
    let pin = transport::read_certificate(peer_certificate)?;
    let endpoint = audio::endpoint(bind, &identity, &pin)?;
    println!(
        "Waiting for the authenticated audio source on {}.",
        endpoint.local_addr()?
    );
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let connection = tokio::select! {
        biased;
        result = &mut stop => {
            result?;
            return Ok(());
        }
        result = tokio::time::timeout(LISTEN_TIMEOUT, async {
            let incoming = endpoint.accept().await.context("audio listener closed")?;
            Ok::<_, anyhow::Error>(incoming.await?)
        }) => result.context("audio listener timed out")??,
    };
    let session = tokio::select! {
        biased;
        result = &mut stop => {
            result?;
            connection.close(0u8.into(), b"playback cancelled");
            return Ok(());
        }
        result = audio::accept(&connection, &pin, audio::Direction::Playback, duration) => result?,
    };
    #[cfg(windows)]
    let device = device
        .map(str::parse::<u32>)
        .transpose()
        .context("Windows audio device must be a numeric output ID")?;
    let mut player = native::Player::open_after_explicit_consent(device)?;
    let result = drive(&session, &mut stop, |samples| {
        player.submit(samples, volume)
    })
    .await;
    session.stop();
    // Attempt device cleanup even when networking or sample submission fails.
    player.stop().context("local audio output cleanup failed")?;
    result
}

async fn drive<F>(
    session: &audio::Session,
    stop: F,
    mut submit: impl FnMut(&[i16; SAMPLES]) -> Result<bool>,
) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let mut buffer = Buffer::default();
    let epoch = buffer
        .start()
        .context("audio output generation exhausted")?;
    let mut output = [0; SAMPLES];
    let mut started = false;
    let mut tick = tokio::time::interval(PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let incoming = session.receive();
    tokio::pin!(stop);
    tokio::pin!(incoming);
    loop {
        tokio::select! {
            biased;
            result = &mut stop => {
                result?;
                buffer.stop();
                return Ok(());
            }
            _ = tick.tick() => {
                let playout = buffer.render(epoch, &mut output);
                started |= playout == Playout::Audio;
                if started {
                    submit(&output)?;
                }
            }
            frame = &mut incoming => {
                let frame = frame?;
                buffer.push(epoch, frame);
                incoming.set(session.receive());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_protocol::audio::Frame;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn authenticated_audio_reaches_local_sink_and_stop_ends_playout() -> Result<()> {
        let source = transport::Identity::generate()?;
        let destination = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = audio::endpoint(bind, &source, &destination.certificate)?;
        let right = audio::endpoint(bind, &destination, &source.certificate)?;
        let (client, server) = tokio::try_join!(
            async {
                let connection = left.connect(right.local_addr()?, "peerglide.local")?;
                Ok::<_, anyhow::Error>(connection.await?)
            },
            async {
                let connection = right.accept().await.context("missing playback peer")?;
                Ok::<_, anyhow::Error>(connection.await?)
            },
        )?;
        let duration = Duration::from_secs(10);
        let (mut source_session, receiver_session) = tokio::try_join!(
            audio::open(
                &client,
                &destination.certificate,
                audio::Direction::Send,
                duration
            ),
            audio::accept(
                &server,
                &source.certificate,
                audio::Direction::Playback,
                duration
            ),
        )?;
        let (stop, stopped) = oneshot::channel::<()>();
        let sender = async {
            let frame = Frame {
                sequence: 1,
                samples: [11; SAMPLES],
            };
            assert!(source_session.send(&frame)?);
            Ok::<_, anyhow::Error>(())
        };
        let mut stop = Some(stop);
        let receiver = drive(
            &receiver_session,
            async {
                stopped
                    .await
                    .map_err(|_| std::io::Error::other("playback test ended"))
            },
            |samples| {
                assert_eq!(*samples, [11; SAMPLES]);
                stop.take().unwrap().send(()).unwrap();
                Ok(true)
            },
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(sender, receiver)
        })
        .await??;
        assert!(stop.is_none());
        Ok(())
    }
}
