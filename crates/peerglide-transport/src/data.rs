//! Independent data negotiation over pinned mutual TLS. Capability is not consent.
use super::data_frames::{FrameReceiver, FrameSender};
pub use super::data_session::{
    AdmittedFrame, AdmittedImage, ClipboardCompletion, Consent, ImageCompletion, Receiver, Sender,
};
use super::{endpoint_alpn, verify_peer, Identity};
use anyhow::{ensure, Context, Result};
use quinn::{crypto::rustls::HandshakeData, Connection, Endpoint, RecvStream, SendStream};
use std::{net::SocketAddr, time::Duration};

pub(super) const ALPN: &[u8] = b"peerglide-data/1";
const MAGIC: &[u8; 4] = b"PGDH";
const VERSION: u8 = 1;
const TEXT: u8 = 1;
const FILES: u8 = 2;
const IMAGES: u8 = 4;
const HELLO_BYTES: usize = 8;
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub text: bool,
    pub files: bool,
    pub images: bool,
}

impl Capabilities {
    fn hello(self) -> [u8; HELLO_BYTES] {
        let flags = (u8::from(self.text) * TEXT)
            | (u8::from(self.files) * FILES)
            | (u8::from(self.images) * IMAGES);
        [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION, flags, 0, 0]
    }

    fn from_hello(hello: [u8; HELLO_BYTES]) -> Result<Self> {
        ensure!(
            &hello[..4] == MAGIC
                && hello[4] == VERSION
                && hello[5] & !(TEXT | FILES | IMAGES) == 0
                && hello[6..] == [0, 0],
            "invalid or unsupported data hello"
        );
        Ok(Self {
            text: hello[5] & TEXT != 0,
            files: hello[5] & FILES != 0,
            images: hello[5] & IMAGES != 0,
        })
    }

    fn shared(self, peer: Self) -> Self {
        Self {
            text: self.text && peer.text,
            files: self.files && peer.files,
            images: self.images && peer.images,
        }
    }
}

/// A single negotiated stream on a data connection. Callers still need
/// independent direction consent before any clipboard or file side effect.
pub struct Stream {
    send: SendStream,
    receive: RecvStream,
    connection: Connection,
    peer_digest: [u8; 32],
    pub capabilities: Capabilities,
}

impl Stream {
    /// Bind a fresh local consent owner and separate I/O halves to this
    /// authenticated data connection. Both directions start with no consent.
    pub fn into_session(
        self,
        send_bytes: u64,
        receive_bytes: u64,
    ) -> Result<(Consent, Sender, Receiver)> {
        let connection = self.connection.clone();
        let peer_digest = self.peer_digest;
        let capabilities = self.capabilities;
        let (sender, receiver) = self.into_frames(send_bytes, receive_bytes);
        super::data_session::new(connection, peer_digest, capabilities, sender, receiver)
    }

    fn into_frames(self, send_bytes: u64, receive_bytes: u64) -> (FrameSender, FrameReceiver) {
        (
            FrameSender::new(
                self.send,
                self.connection.clone(),
                self.capabilities,
                send_bytes,
            ),
            FrameReceiver::new(
                self.receive,
                self.connection,
                self.capabilities,
                receive_bytes,
            ),
        )
    }
}

#[derive(Clone, Copy)]
enum Side {
    Connector,
    Listener,
}

/// Opens an endpoint with a data ALPN only, independently of the input endpoint.
pub fn endpoint(address: SocketAddr, local: &Identity, pinned_peer: &[u8]) -> Result<Endpoint> {
    ensure!(
        local.certificate.as_slice() != pinned_peer,
        "data peer must have a distinct identity"
    );
    endpoint_alpn(address, local, pinned_peer, ALPN)
}

pub async fn open(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
) -> Result<Stream> {
    negotiate(connection, pinned_peer, supported, Side::Connector).await
}

pub async fn accept(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
) -> Result<Stream> {
    negotiate(connection, pinned_peer, supported, Side::Listener).await
}

async fn negotiate(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
    side: Side,
) -> Result<Stream> {
    let handshake = connection
        .handshake_data()
        .context("data TLS handshake is unavailable")?
        .downcast::<HandshakeData>()
        .map_err(|_| anyhow::anyhow!("unexpected data TLS handshake"))?;
    ensure!(
        handshake.protocol.as_deref() == Some(ALPN),
        "unexpected data ALPN"
    );
    verify_peer(connection, pinned_peer)?;

    let result = tokio::time::timeout(NEGOTIATION_TIMEOUT, async {
        let (mut send, mut receive) = match side {
            Side::Connector => connection.open_bi().await?,
            Side::Listener => connection.accept_bi().await?,
        };
        ensure!(
            send.id().index() == 0,
            "data connection already used a stream"
        );
        let mut hello = [0; HELLO_BYTES];
        let peer = match side {
            Side::Connector => {
                send.write_all(&supported.hello()).await?;
                receive.read_exact(&mut hello).await?;
                Capabilities::from_hello(hello)?
            }
            Side::Listener => {
                receive.read_exact(&mut hello).await?;
                let peer = Capabilities::from_hello(hello)?;
                send.write_all(&supported.hello()).await?;
                peer
            }
        };
        Ok(Stream {
            send,
            receive,
            connection: connection.clone(),
            peer_digest: *blake3::hash(pinned_peer).as_bytes(),
            capabilities: supported.shared(peer),
        })
    })
    .await
    .context("data negotiation timed out")
    .and_then(|result| result);
    if result.is_err() {
        connection.close(1u8.into(), b"data negotiation failed");
    }
    result
}

#[cfg(test)]
// Generated 1x1 RGBA test image.
pub(crate) const TEST_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00,
    0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
    0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0x10, 0x54, 0x32, 0xfe, 0x0f, 0x00,
    0x02, 0x14, 0x01, 0x66, 0x04, 0xe0, 0xde, 0x7f, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT_ONLY: Capabilities = Capabilities {
        text: true,
        files: false,
        images: false,
    };
    const TEXT_AND_FILES: Capabilities = Capabilities {
        text: true,
        files: true,
        images: false,
    };

    async fn connect(client: &Endpoint, server: &Endpoint) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let outbound = async {
                let connection = client.connect(server.local_addr()?, "peerglide.local")?;
                Ok::<_, anyhow::Error>(connection.await?)
            };
            let inbound = async {
                let connection = server.accept().await.context("no incoming connection")?;
                Ok::<_, anyhow::Error>(connection.await?)
            };
            tokio::try_join!(outbound, inbound)
        })
        .await?
    }

    #[test]
    fn unknown_hello_fields_fail_closed() {
        let hello = TEXT_AND_FILES.hello();
        assert_eq!(Capabilities::from_hello(hello).unwrap(), TEXT_AND_FILES);
        for (index, value) in [(0, b'X'), (4, 2), (5, 0x80), (7, 1)] {
            let mut invalid = hello;
            invalid[index] = value;
            assert!(Capabilities::from_hello(invalid).is_err());
        }
    }

    #[test]
    fn image_bit_requires_both_endpoints_and_preserves_text_file_hello() {
        let all = Capabilities { images: true, ..TEXT_AND_FILES };
        assert_eq!(TEXT_AND_FILES.hello()[5], 3);
        assert_eq!(Capabilities::from_hello(all.hello()).unwrap(), all);
        assert_eq!(all.shared(TEXT_ONLY), TEXT_ONLY);
        assert_eq!(all.shared(all), all);
    }

    #[tokio::test]
    async fn data_negotiates_only_shared_capabilities_with_exact_pins() -> Result<()> {
        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let other = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &local, &peer.certificate)?;
        let right = endpoint(bind, &peer, &local.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        assert!(open(&connector, &other.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(connector.max_datagram_size().is_none());
        let (client, server) = tokio::try_join!(
            open(&connector, &peer.certificate, TEXT_ONLY),
            accept(&listener, &local.certificate, TEXT_AND_FILES)
        )?;
        assert_eq!(client.capabilities, TEXT_ONLY);
        assert_eq!(server.capabilities, TEXT_ONLY);
        Ok(())
    }

    #[tokio::test]
    async fn admitted_data_roundtrip_consumes_exact_wire_budgets() -> Result<()> {
        use peerglide_transfer::codec::{Frame, Kind, HEADER_BYTES};

        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &local, &peer.certificate)?;
        let right = endpoint(bind, &peer, &local.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        let (outbound, inbound) = tokio::try_join!(
            open(&connector, &peer.certificate, TEXT_ONLY),
            accept(&listener, &local.certificate, TEXT_ONLY)
        )?;
        let text = "屏跃";
        let limit = (HEADER_BYTES + 8 + text.len()) as u64;
        let (mut send, _) = outbound.into_frames(limit, 0);
        let (_, mut receive) = inbound.into_frames(0, limit);
        let frame = Frame::Text {
            sequence: 1,
            text: text.into(),
        };
        let ((), (received, ticket)) = tokio::try_join!(
            send.write_checked(&frame, |kind| {
                assert_eq!(kind, Kind::Text);
                Ok(())
            }),
            receive.read_checked(|header| {
                assert_eq!(header.kind(), Kind::Text);
                Ok(23)
            })
        )?;
        assert_eq!(ticket, 23);
        assert!(matches!(
            received,
            Frame::Text { sequence: 1, text: received_text } if received_text == text
        ));
        assert_eq!(send.remaining_bytes(), 0);
        assert_eq!(receive.remaining_bytes(), 0);
        assert!(send.write_checked(&frame, |_| Ok(())).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn rejected_or_cancelled_frame_closes_without_the_body() -> Result<()> {
        use peerglide_transfer::codec::{Frame, Kind, HEADER_BYTES};

        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let mut wire = Vec::new();
        let frame = Frame::Text {
            sequence: 1,
            text: "test".into(),
        };
        frame.write(&mut wire)?;
        for allow_header in [false, true] {
            let left = endpoint(bind, &local, &peer.certificate)?;
            let right = endpoint(bind, &peer, &local.certificate)?;
            let (connector, listener) = connect(&left, &right).await?;
            let (mut outbound, inbound) = tokio::try_join!(
                open(&connector, &peer.certificate, TEXT_ONLY),
                accept(&listener, &local.certificate, TEXT_ONLY)
            )?;
            let (_, mut receive) = inbound.into_frames(0, 4096);
            outbound.send.write_all(&wire[..HEADER_BYTES]).await?;
            if allow_header {
                let (admitted, seen) = tokio::sync::oneshot::channel();
                let read = receive.read_checked(|header| {
                    assert_eq!(header.kind(), Kind::Text);
                    admitted.send(()).unwrap();
                    Ok(())
                });
                tokio::pin!(read);
                tokio::select! {
                    signal = seen => signal?,
                    _ = &mut read => panic!("a frame without its body completed"),
                }
                // Leaving this scope cancels the partially read frame.
            } else {
                let result = receive
                    .read_checked::<()>(|header| {
                        assert_eq!(header.kind(), Kind::Text);
                        anyhow::bail!("direction not allowed");
                    })
                    .await;
                let error = result.err().context("rejected frame was received")?;
                assert_eq!(error.to_string(), "direction not allowed");
            }
            assert!(listener.close_reason().is_some());
        }
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_clipboard_checks_consent_revision_and_revocation() -> Result<()> {
        use peerglide_transfer::{
            clipboard::{Backend, Classification, Origin, WriteResult},
            codec::TextOutcome,
            data_policy::Permissions,
        };

        struct Memory {
            revision: u64,
            text: String,
        }
        impl Backend for Memory {
            fn compare_and_write(&mut self, expected: u64, text: &str) -> WriteResult {
                if self.revision != expected {
                    return WriteResult::Changed;
                }
                self.text = text.into();
                self.revision += 1;
                WriteResult::Submitted
            }
        }
        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &local, &peer.certificate)?;
        let right = endpoint(bind, &peer, &local.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        let (outbound, inbound) = tokio::try_join!(
            open(&connector, &peer.certificate, TEXT_ONLY),
            accept(&listener, &local.certificate, TEXT_ONLY)
        )?;
        let (mut local_consent, mut send, mut local_receive) = outbound.into_session(4096, 4096)?;
        let (mut peer_consent, mut peer_send, mut receive) = inbound.into_session(4096, 4096)?;
        let share = Classification::ShareableText;
        let origin = Origin::Local;
        assert!(send.write_text("屏跃", share, origin).await.is_err());
        local_consent.grant(
            Permissions {
                text_send: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        peer_consent.grant(
            Permissions {
                text_receive: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        assert!(peer_send
            .write_text("未授予发送", share, origin)
            .await
            .is_err());
        assert!(send
            .write_text("仅本机", Classification::LocalOnly, origin)
            .await
            .is_err());

        let mut backend = Memory {
            revision: 7,
            text: String::new(),
        };
        let (sequence, mut first) = tokio::try_join!(
            send.write_text("屏跃", share, origin),
            receive.read(Some(backend.revision))
        )?;
        tokio::try_join!(
            peer_send.acknowledge_text(&mut first),
            local_receive.wait_text_receipt(sequence)
        )?;
        assert!(peer_send.acknowledge_text(&mut first).await.is_err());
        assert!(backend.text.is_empty());
        let completed = peer_consent.apply_text(first, &mut backend)?;
        assert_eq!(completed.outcome(), TextOutcome::Submitted);
        let ((), result) = tokio::try_join!(
            peer_send.report_text_result(completed),
            local_receive.wait_text_result(sequence)
        )?;
        assert_eq!(result, TextOutcome::Submitted);
        assert_eq!(backend.text, "屏跃");

        tokio::time::sleep(Duration::from_millis(110)).await;
        let (_, second) = tokio::try_join!(
            send.write_text("第二帧", share, origin),
            receive.read(Some(backend.revision))
        )?;
        backend.revision += 1;
        backend.text = "接收期间的本机编辑".into();
        let changed = peer_consent.apply_text(second, &mut backend)?;
        assert_eq!(changed.outcome(), TextOutcome::Changed);
        assert_eq!(backend.text, "接收期间的本机编辑");

        tokio::time::sleep(Duration::from_millis(110)).await;
        let (declined_sequence, declined) = tokio::try_join!(
            send.write_text("拒绝样本", share, origin),
            receive.read(Some(backend.revision))
        )?;
        let declined = peer_consent.decline_text(declined)?;
        let ((), result) = tokio::try_join!(
            peer_send.report_text_result(declined),
            local_receive.wait_text_result(declined_sequence)
        )?;
        assert_eq!(result, TextOutcome::Declined);
        assert_eq!(backend.text, "接收期间的本机编辑");

        tokio::time::sleep(Duration::from_millis(110)).await;
        let (_, third) = tokio::try_join!(
            send.write_text("第三帧", share, origin),
            receive.read(Some(backend.revision))
        )?;
        peer_consent.revoke();
        assert!(peer_consent.apply_text(third, &mut backend).is_err());
        assert_eq!(backend.text, "接收期间的本机编辑");
        assert!(listener.close_reason().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_clipboard_receipt_closes_the_data_connection() -> Result<()> {
        use peerglide_transfer::{
            clipboard::{Classification, Origin},
            data_policy::Permissions,
        };

        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &local, &peer.certificate)?;
        let right = endpoint(bind, &peer, &local.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        let (outbound, inbound) = tokio::try_join!(
            open(&connector, &peer.certificate, TEXT_ONLY),
            accept(&listener, &local.certificate, TEXT_ONLY)
        )?;
        let (mut local_consent, mut send, mut local_receive) = outbound.into_session(4096, 4096)?;
        let (mut peer_consent, mut peer_send, mut receive) = inbound.into_session(4096, 4096)?;
        local_consent.grant(
            Permissions {
                text_send: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        peer_consent.grant(
            Permissions {
                text_receive: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        let (sequence, mut admitted) = tokio::try_join!(
            send.write_text("样本", Classification::ShareableText, Origin::Local),
            receive.read(Some(7))
        )?;
        peer_send.acknowledge_text(&mut admitted).await?;
        let error = local_receive
            .wait_text_receipt(sequence + 1)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "clipboard receipt sequence differs");
        assert!(connector.close_reason().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn file_responses_use_reverse_direction_and_chunks_wait_for_the_correct_phase(
    ) -> Result<()> {
        use peerglide_transfer::{
            codec::{FileOutcome, Frame, Kind},
            data_policy::Permissions,
        };

        let source = Identity::generate()?;
        let destination = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &source, &destination.certificate)?;
        let right = endpoint(bind, &destination, &source.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        let (outbound, inbound) = tokio::try_join!(
            open(&connector, &destination.certificate, TEXT_AND_FILES),
            accept(&listener, &source.certificate, TEXT_AND_FILES)
        )?;
        let (mut source_consent, mut source_send, mut source_receive) =
            outbound.into_session(4096, 4096)?;
        let (mut destination_consent, mut destination_send, mut destination_receive) =
            inbound.into_session(4096, 4096)?;
        source_consent.grant(
            Permissions {
                file_send: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        let id = [21; 16];
        let response = Frame::FileResult {
            id,
            outcome: FileOutcome::Declined,
        };
        assert!(destination_send.write(&response).await.is_err());
        assert!(source_send.write(&response).await.is_err());
        destination_consent.grant(
            Permissions {
                file_receive: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        let timeout = Duration::from_secs(5);
        let ((), admitted) = tokio::try_join!(
            destination_send.write(&response),
            source_receive.read_file(&[Kind::FileResult], timeout)
        )?;
        assert!(source_consent.apply_received(admitted, |frame| Ok(matches!(
            frame,
            Frame::FileResult { id: received, outcome: FileOutcome::Declined } if received == id
        )))?);
        source_send
            .write(&Frame::Chunk {
                id,
                index: 0,
                bytes: vec![5],
            })
            .await?;
        let error = destination_receive
            .read_file(&[Kind::Offer], timeout)
            .await
            .err()
            .context("unexpectedly admitted a chunk before an offer")?;
        assert_eq!(
            error.to_string(),
            "unexpected frame for the current file transfer phase"
        );
        assert!(listener.close_reason().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_image_import_rechecks_revision_and_revocation() -> Result<()> {
        use peerglide_transfer::{
            clipboard::{Origin, WriteResult},
            clipboard_image::{Image, ImageBackend},
            codec::ClipboardOutcome,
            data_policy::Permissions,
        };

        struct Memory {
            revision: u64,
            writes: usize,
        }

        impl ImageBackend for Memory {
            fn compare_and_write_image(&mut self, expected: u64, image: &Image) -> WriteResult {
                if expected != self.revision {
                    return WriteResult::Changed;
                }
                assert_eq!(image.dimensions(), (1, 1));
                self.revision += 1;
                self.writes += 1;
                WriteResult::Submitted
            }
        }

        let source = Identity::generate()?;
        let destination = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &source, &destination.certificate)?;
        let right = endpoint(bind, &destination, &source.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        let images = Capabilities {
            images: true,
            ..Capabilities::default()
        };
        let (outbound, inbound) = tokio::try_join!(
            open(&connector, &destination.certificate, images),
            accept(&listener, &source.certificate, images),
        )?;
        let (mut sender_consent, mut send, mut replies) = outbound.into_session(4096, 4096)?;
        let (mut receiver_consent, mut respond, mut receive) = inbound.into_session(4096, 4096)?;
        sender_consent.grant(
            Permissions {
                image_send: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        receiver_consent.grant(
            Permissions {
                image_receive: true,
                ..Permissions::default()
            },
            60_000,
        )?;
        assert!(send.write_image(Image::normalize(&TEST_PNG)?, Origin::Imported).await.is_err());
        let mut backend = Memory { revision: 9, writes: 0 };
        for outcome in [ClipboardOutcome::Submitted, ClipboardOutcome::Changed] {
            let (sequence, pending) = tokio::try_join!(
                send.write_image(Image::normalize(&TEST_PNG)?, Origin::Local),
                receive.read_image(9),
            )?;
            assert_eq!(pending.dimensions(), (1, 1));
            let completion = receiver_consent.apply_image(pending, &mut backend)?;
            assert_eq!(completion.outcome(), outcome);
            tokio::try_join!(
                respond.report_image_result(completion),
                async {
                    assert_eq!(replies.wait_image_result(sequence).await?, outcome);
                    Ok::<_, anyhow::Error>(())
                },
            )?;
        }
        assert_eq!(backend.writes, 1);

        let (_, pending) = tokio::try_join!(
            send.write_image(Image::normalize(&TEST_PNG)?, Origin::Local),
            receive.read_image(backend.revision),
        )?;
        receiver_consent.revoke();
        assert!(receiver_consent.apply_image(pending, &mut backend).is_err());
        assert_eq!(backend.writes, 1);
        Ok(())
    }

    #[tokio::test]
    async fn input_and_data_connections_cannot_be_crossed() -> Result<()> {
        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let data = endpoint(bind, &local, &peer.certificate)?;
        let incompatible = super::super::endpoint(bind, &peer, &local.certificate)?;
        assert!(connect(&data, &incompatible).await.is_err());

        let input = super::super::endpoint(bind, &peer, &local.certificate)?;
        let input_client = super::super::endpoint(bind, &local, &peer.certificate)?;
        let (connector, listener) = connect(&input_client, &input).await?;
        assert!(open(&connector, &peer.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(accept(&listener, &local.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(connector.close_reason().is_none() && listener.close_reason().is_none());
        Ok(())
    }
}
