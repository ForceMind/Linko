//! Bounded packet assembly for an explicitly authorized system-audio source.
use anyhow::{ensure, Result};
use peerglide_protocol::audio::SAMPLES;

struct Packet {
    samples: [i16; SAMPLES],
    length: usize,
}

impl Packet {
    fn new() -> Self {
        Self {
            samples: [0; SAMPLES],
            length: 0,
        }
    }

    fn fill(
        &mut self,
        mut read: impl FnMut(&mut [i16]) -> Result<usize>,
    ) -> Result<[i16; SAMPLES]> {
        let mut fresh = false;
        while self.length < SAMPLES {
            let available = &mut self.samples[self.length..];
            let count = read(available)?;
            ensure!(
                count <= available.len() && count.is_multiple_of(2),
                "invalid native PCM sample count"
            );
            if count == 0 {
                // Keep a partial packet only if this period got fresh input.
                // Silence must not carry old samples across an idle gap.
                if !fresh {
                    self.length = 0;
                }
                return Ok([0; SAMPLES]);
            }
            fresh = true;
            self.length += count;
        }
        self.length = 0;
        Ok(self.samples)
    }
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
#[derive(Clone, Copy)]
enum Source {
    System,
    Microphone,
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
pub(super) async fn share_system(
    identity: &std::path::Path,
    peer_certificate: &std::path::Path,
    address: std::net::SocketAddr,
    duration: std::time::Duration,
) -> Result<()> {
    share(Source::System, identity, peer_certificate, address, duration).await
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
pub(super) async fn share_microphone(
    identity: &std::path::Path,
    peer_certificate: &std::path::Path,
    address: std::net::SocketAddr,
    duration: std::time::Duration,
) -> Result<()> {
    share(Source::Microphone, identity, peer_certificate, address, duration).await
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
async fn share(
    source: Source,
    identity: &std::path::Path,
    peer_certificate: &std::path::Path,
    address: std::net::SocketAddr,
    duration: std::time::Duration,
) -> Result<()> {
    use anyhow::Context;
    use peerglide_protocol::audio::Frame;
    use peerglide_transport::{self as transport, audio};
    use std::{
        net::{Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    };

    let identity = transport::Identity::load(identity)?;
    let pin = transport::read_certificate(peer_certificate)?;
    let bind = if address.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let endpoint = match source {
        Source::System => audio::endpoint(bind, &identity, &pin)?,
        Source::Microphone => audio::microphone::endpoint(bind, &identity, &pin)?,
    };
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let connection = tokio::select! {
        biased;
        result = &mut stop => {
            result?;
            return Ok(());
        }
        result = tokio::time::timeout(
            Duration::from_secs(8),
            endpoint.connect(address, "peerglide.local")?,
        ) => result.context("audio connection timed out")??,
    };
    let mut session = tokio::select! {
        biased;
        result = &mut stop => {
            result?;
            connection.close(0u8.into(), b"audio cancelled");
            return Ok(());
        }
        result = async {
            match source {
                Source::System => audio::open(&connection, &pin, audio::Direction::Send, duration).await,
                Source::Microphone => audio::microphone::open(&connection, &pin, duration).await,
            }
        } => result?,
    };
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_capture = stopped.clone();
    let (samples, mut received) = tokio::sync::mpsc::channel(2);
    let worker = std::thread::Builder::new()
        .name("linko-audio-source".into())
        .spawn(move || capture_samples(source, &stop_capture, samples))?;
    let mut sequence = 0u64;
    let result = async {
        loop {
            tokio::select! {
                biased;
                result = &mut stop => return result.map_err(anyhow::Error::from),
                _ = connection.closed() => anyhow::bail!("audio playback peer disconnected"),
                packet = received.recv() => {
                    let Some(samples) = packet else { return Ok(()); };
                    sequence = sequence.checked_add(1).context("audio sequence exhausted")?;
                    session.send(&Frame { sequence, samples })?;
                }
            }
        }
    }
    .await;
    stopped.store(true, Ordering::Release);
    drop(received);
    session.stop();
    tokio::task::spawn_blocking(move || {
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("local audio worker ended unexpectedly"))?
    })
    .await??;
    result
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
fn capture_samples(
    source: Source,
    stopped: &std::sync::atomic::AtomicBool,
    samples: tokio::sync::mpsc::Sender<[i16; SAMPLES]>,
) -> Result<()> {
    use anyhow::Context;
    use peerglide_platform::system_audio_capture::AudioCapture;
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    use tokio::sync::mpsc::error::TrySendError;

    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    let opened = match source {
        Source::System => AudioCapture::open_after_explicit_consent(),
        Source::Microphone => AudioCapture::open_microphone_after_explicit_consent(stopped),
    };
    if stopped.load(Ordering::Acquire) {
        if let Ok(mut capture) = opened {
            capture.stop().context("local audio capture cleanup failed")?;
        }
        return Ok(());
    }
    let mut capture = opened?;
    let mut packet = Packet::new();
    let period = Duration::from_millis(5);
    let result = (|| -> Result<()> {
        let mut next = Instant::now();
        while !stopped.load(Ordering::Acquire) {
            let pcm = packet.fill(|output| capture.read(output))?;
            if matches!(samples.try_send(pcm), Err(TrySendError::Closed(_))) {
                break;
            }
            let now = Instant::now();
            next += period;
            if next <= now {
                next = now + period;
            }
            std::thread::sleep(next.saturating_duration_since(now));
        }
        Ok(())
    })();
    capture
        .stop()
        .context("local audio capture cleanup failed")?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_native_samples_are_retained_across_one_silent_period() -> Result<()> {
        let mut packet = Packet::new();
        let mut reads = 0;
        let silence = packet.fill(|output| {
            reads += 1;
            if reads == 1 {
                output[..2].copy_from_slice(&[7, -7]);
                Ok(2)
            } else {
                Ok(0)
            }
        })?;
        assert_eq!(silence, [0; SAMPLES]);
        let audio = packet.fill(|output| {
            output.fill(21);
            Ok(output.len())
        })?;
        assert_eq!(audio[..2], [7, -7]);
        assert!(audio[2..].iter().all(|sample| *sample == 21));
        assert_eq!(packet.fill(|_| Ok(0))?, [0; SAMPLES]);
        Ok(())
    }

    #[test]
    fn an_idle_period_retires_unfinished_audio() -> Result<()> {
        let mut packet = Packet::new();
        let mut reads = 0;
        packet.fill(|output| {
            reads += 1;
            if reads == 1 {
                output[..2].fill(19);
                Ok(2)
            } else {
                Ok(0)
            }
        })?;
        assert_eq!(packet.length, 2);
        assert_eq!(packet.fill(|_| Ok(0))?, [0; SAMPLES]);
        assert_eq!(packet.length, 0);
        Ok(())
    }

    #[test]
    fn malformed_sample_count_does_not_advance_the_packet() {
        let mut packet = Packet::new();
        assert!(packet.fill(|_| Ok(1)).is_err());
        assert!(packet.fill(|output| Ok(output.len() + 2)).is_err());
        assert_eq!(packet.length, 0);
    }
}
