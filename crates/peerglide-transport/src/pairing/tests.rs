use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn real_tcp_pake_then_mutual_tls_returns_only_bound_temporary_peers() -> Result<()> {
    let server = Arc::new(Identity::generate()?);
    let client = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&server)?;
    let entry = CodeEntry::new(offer.code()?.to_owned())?;
    let cancel_client = entry.cancellation();
    let cancel_server = offer.cancellation();
    let (accepted, connected) = tokio::try_join!(
        offer.accept(&listener, server.clone()),
        entry.connect(listener.local_addr()?, client.clone()),
    )?;
    assert_eq!(accepted.peer_certificate()?, client.certificate);
    assert_eq!(connected.peer_certificate()?, server.certificate);
    assert_eq!(accepted.context_id()?, connected.context_id()?);
    assert!(offer.code().is_err());
    assert_eq!(offer.attempts_remaining(), ATTEMPT_LIMIT - 1);
    cancel_client.cancel();
    assert!(connected.peer_certificate().is_err());
    cancel_server.cancel();
    assert!(accepted.peer_certificate().is_err());
    Ok(())
}

#[tokio::test]
async fn wrong_code_fails_both_sides_without_a_tls_or_peer_proposal() -> Result<()> {
    let server = Arc::new(Identity::generate()?);
    let client = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&server)?;
    let wrong = if offer.code()? == "12345678" { "87654321" } else { "12345678" };
    let entry = CodeEntry::new(wrong.to_owned())?;
    let (a, b) = tokio::join!(offer.accept(&listener, server), entry.connect(listener.local_addr()?, client));
    assert!(a.is_err() && b.is_err());
    assert_eq!(offer.attempts_remaining(), ATTEMPT_LIMIT - 1);
    Ok(())
}

#[tokio::test]
async fn malformed_and_disconnected_sockets_share_one_offer_budget() -> Result<()> {
    let server = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&server)?;
    for _ in 0..ATTEMPT_LIMIT {
        let invalid = async {
            let mut socket = TcpStream::connect(listener.local_addr()?).await?;
            socket.write_u32(u32::MAX).await?;
            Ok::<_, anyhow::Error>(())
        };
        let (a, b) = tokio::join!(offer.accept(&listener, server.clone()), invalid);
        assert!(a.is_err());
        b?;
    }
    assert_eq!(offer.attempts_remaining(), 0);
    assert!(offer.code().is_err());
    // Must reject before waiting for a sixth socket or starting expensive work.
    assert!(tokio::time::timeout(Duration::from_millis(100), offer.accept(&listener, server)).await?.is_err());
    Ok(())
}

#[tokio::test]
async fn cancellation_and_absolute_expiry_reject_before_connect_or_accept() -> Result<()> {
    let identity = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&identity)?;
    offer.cancellation().cancel();
    assert!(offer.accept(&listener, identity.clone()).await.is_err());
    let entry = CodeEntry::new("12345678".into())?;
    entry.cancellation().cancel();
    assert!(entry.connect(listener.local_addr()?, identity.clone()).await.is_err());
    let mut offer = Offer::new(&identity)?;
    offer.lifetime.deadline = Instant::now();
    assert!(offer.code().is_err());
    assert!(offer.accept(&listener, identity).await.is_err());
    Ok(())
}

#[tokio::test]
async fn idle_incoming_handshake_cancels_without_waiting_for_socket_timeout() -> Result<()> {
    let identity = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&identity)?;
    let cancel = offer.cancellation();
    let client = async {
        let socket = TcpStream::connect(listener.local_addr()?).await?;
        cancel.cancel();
        // Keep the TCP connection open: cancellation must close the handshake.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(socket);
        Ok::<_, anyhow::Error>(())
    };
    let (server, client) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(offer.accept(&listener, identity), client)
    }).await?;
    assert!(server.is_err());
    client?;
    Ok(())
}

#[tokio::test]
async fn exact_reads_preserve_pipelined_tls_prefix_and_reject_trailing_hello() -> Result<()> {
    let id = Identity::generate()?;
    let hello = wire::hello(&random_id(), &id.certificate)?;
    let mut trailing = hello.clone();
    trailing.push(0);
    assert!(wire::parse_hello(&trailing).is_err());
    for size in 0..hello.len() { assert!(wire::parse_hello(&hello[..size]).is_err()); }
    let (mut a, mut b) = tokio::io::duplex(4096);
    let writer = async {
        wire::write(&mut a, wire::KE3, b"finalization").await?;
        a.write_all(b"TLS ClientHello prefix").await?;
        Ok::<_, anyhow::Error>(())
    };
    let reader = async {
        assert_eq!(wire::read(&mut b, wire::KE3, 1024).await?, b"finalization");
        let mut prefix = [0; 22];
        tokio::io::AsyncReadExt::read_exact(&mut b, &mut prefix).await?;
        assert_eq!(&prefix, b"TLS ClientHello prefix");
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(writer, reader)?;
    Ok(())
}

#[test]
fn canonical_binding_changes_with_each_identity_role_offer_and_attempt() -> Result<()> {
    let a = Identity::generate()?;
    let b = Identity::generate()?;
    let offer = random_id();
    let attempt = random_id();
    let original = Binding::new(&a.certificate, &b.certificate, offer, attempt)?;
    for other in [
        Binding::new(&b.certificate, &a.certificate, offer, attempt)?,
        Binding::new(&a.certificate, &b.certificate, random_id(), attempt)?,
        Binding::new(&a.certificate, &b.certificate, offer, random_id())?,
    ] {
        assert_ne!(original.context, other.context);
        assert!(wire::verify_ready(&other.context, &original).is_err());
    }
    assert!(Binding::new(&a.certificate, &a.certificate, offer, attempt).is_err());
    for bad in ["", "1234567", "123456789", "１２３４５６７８", "abcdefgh"] {
        assert!(CodeEntry::new(bad.into()).is_err());
    }
    Ok(())
}
