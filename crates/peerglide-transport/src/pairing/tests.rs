use super::*;
use tokio::io::AsyncWriteExt;

// Production deadlines stay unchanged. Serialize whole KSF-heavy scenarios in
// this single-process test runner so unrelated scenarios do not consume them.
static CRYPTO_SCENARIOS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn real_tcp_pake_then_mutual_tls_returns_only_bound_temporary_peers() -> Result<()> {
    let _scenario = CRYPTO_SCENARIOS.lock().await;
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
    // Returning from an actor that owns Offer must not invalidate its result.
    drop(offer);
    assert_eq!(accepted.peer_certificate()?, client.certificate);
    cancel_client.cancel();
    assert!(connected.peer_certificate().is_err());
    cancel_server.cancel();
    assert!(accepted.peer_certificate().is_err());
    Ok(())
}

#[tokio::test]
async fn wrong_code_fails_both_sides_without_a_tls_or_peer_proposal() -> Result<()> {
    let _scenario = CRYPTO_SCENARIOS.lock().await;
    let server = Arc::new(Identity::generate()?);
    let client = Arc::new(Identity::generate()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut offer = Offer::new(&server)?;
    let wrong = if offer.code()? == "12345678" {
        "87654321"
    } else {
        "12345678"
    };
    let entry = CodeEntry::new(wrong.to_owned())?;
    let (a, b) = tokio::join!(
        offer.accept(&listener, server),
        entry.connect(listener.local_addr()?, client)
    );
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
    assert!(
        tokio::time::timeout(Duration::from_millis(100), offer.accept(&listener, server))
            .await?
            .is_err()
    );
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
    assert!(entry
        .connect(listener.local_addr()?, identity.clone())
        .await
        .is_err());
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
    })
    .await?;
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
    for size in 0..hello.len() {
        assert!(wire::parse_hello(&hello[..size]).is_err());
    }
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

#[tokio::test]
async fn opaque_rejects_parameter_substitution_replay_and_noncanonical_messages() -> Result<()> {
    let _scenario = CRYPTO_SCENARIOS.lock().await;
    let client = Identity::generate()?;
    let server = Identity::generate()?;
    let stranger = Identity::generate()?;
    let offer = random_id();
    let attempt = random_id();
    let binding = Binding::new(&client.certificate, &server.certificate, offer, attempt)?;
    crypto::compute(move || {
        let code = b"12345678";
        let (client_state, ke1) = crypto::client_start(code)?;
        let mut extra = ke1.clone();
        extra.push(0);
        assert!(crypto::server_start(code, &binding, &extra).is_err());
        assert!(crypto::server_start(code, &binding, &ke1[..ke1.len() - 1]).is_err());
        let (server_state, ke2) = crypto::server_start(code, &binding, &ke1)?;
        let mut extra = ke2.clone();
        extra.push(0);
        assert!(crypto::client_finish(client_state.clone(), code, &binding, &extra).is_err());
        assert!(crypto::client_finish(client_state.clone(), code, &binding, &ke2[..ke2.len() - 1]).is_err());
        // These exercise actual OPAQUE verification, not just Binding equality.
        for replaced in [
            Binding::new(&stranger.certificate, &server.certificate, offer, attempt)?,
            Binding::new(&client.certificate, &stranger.certificate, offer, attempt)?,
            Binding::new(&server.certificate, &client.certificate, offer, attempt)?,
            Binding::new(&client.certificate, &server.certificate, random_id(), attempt)?,
            Binding::new(&client.certificate, &server.certificate, offer, random_id())?,
        ] {
            assert!(crypto::client_finish(client_state.clone(), code, &replaced, &ke2).is_err());
        }
        let ke3 = crypto::client_finish(client_state, code, &binding, &ke2)?;
        let mut extra = ke3.clone();
        extra.push(0);
        assert!(crypto::server_finish(server_state.clone(), &binding, &extra).is_err());
        assert!(crypto::server_finish(server_state.clone(), &binding, &ke3[..ke3.len() - 1]).is_err());
        let other = Binding::new(&client.certificate, &server.certificate, offer, random_id())?;
        assert!(crypto::server_finish(server_state.clone(), &other, &ke3).is_err());
        crypto::server_finish(server_state, &binding, &ke3)?;
        let (_, next_ke1) = crypto::client_start(code)?;
        let (next_server, _) = crypto::server_start(code, &binding, &next_ke1)?;
        assert!(crypto::server_finish(next_server, &binding, &ke3).is_err());
        Ok(())
    }).await
}

#[tokio::test]
async fn actual_tls_rejects_wrong_pins_absent_client_identity_and_bad_or_missing_alpn() -> Result<()> {
    for case in 0..5 {
        let server = Identity::generate()?;
        let client = Identity::generate()?;
        let stranger = Identity::generate()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let server_config = server_config(&server, if case == 0 { &stranger.certificate } else { &client.certificate })?;
        let mut client_config = client_config(&client, if case == 1 { &stranger.certificate } else { &server.certificate })?;
        if case == 2 {
            client_config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_root_certificates(crate::roots(&server.certificate)?)
                .with_no_client_auth();
            client_config.alpn_protocols = vec![ALPN.to_vec()];
        } else if case == 3 {
            client_config.alpn_protocols = vec![b"peerglide/5".to_vec()];
        } else if case == 4 {
            client_config.alpn_protocols.clear();
        }
        let accept = async {
            let (socket, _) = listener.accept().await?;
            let stream = TlsAcceptor::from(Arc::new(server_config)).accept(socket).await?;
            let stream = TlsStream::Server(stream);
            verify_tls(&stream, &client.certificate)?;
            Ok::<_, anyhow::Error>(stream)
        };
        let connect = async {
            let socket = TcpStream::connect(listener.local_addr()?).await?;
            let stream = TlsConnector::from(Arc::new(client_config))
                .connect(rustls::pki_types::ServerName::try_from("peerglide.local")?, socket).await?;
            let stream = TlsStream::Client(stream);
            verify_tls(&stream, &server.certificate)?;
            Ok::<_, anyhow::Error>(stream)
        };
        let (a, b) = tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(accept, connect) }).await?;
        assert!(a.is_err() || b.is_err(), "invalid TLS case {case} accepted");
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_ke3_and_before_peer_ready_never_returns_a_session() -> Result<()> {
    let _scenario = CRYPTO_SCENARIOS.lock().await;
    for stage in 0..2 {
        let server = Arc::new(Identity::generate()?);
        let client = Arc::new(Identity::generate()?);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let entry = CodeEntry::new("12345678".into())?;
        let cancel = entry.cancellation();
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server_task = async {
            let (mut socket, _) = listener.accept().await?;
            let hello = wire::read(&mut socket, wire::CLIENT_HELLO, MAX_FRAME).await?;
            let (attempt, peer) = wire::parse_hello(&hello)?;
            let offer = random_id();
            let binding = Binding::new(&peer, &server.certificate, offer, attempt)?;
            wire::write(&mut socket, wire::SERVER_HELLO, &wire::hello(&offer, &server.certificate)?).await?;
            let ke1 = wire::read(&mut socket, wire::KE1, 1024).await?;
            let bound = binding.clone();
            let (state, ke2) = crypto::compute(move || crypto::server_start(b"12345678", &bound, &ke1)).await?;
            wire::write(&mut socket, wire::KE2, &ke2).await?;
            let ke3 = wire::read(&mut socket, wire::KE3, 1024).await?;
            crypto::server_finish(state, &binding, &ke3)?;
            if stage == 0 {
                let _ = reached_tx.send(());
                release_rx.await?;
                drop(socket);
            } else {
                let tls = TlsAcceptor::from(Arc::new(server_config(&server, &peer)?)).accept(socket).await?;
                let _ = reached_tx.send(());
                release_rx.await?;
                drop(tls);
            }
            Ok::<_, anyhow::Error>(())
        };
        let cancel_task = async {
            reached_rx.await?;
            cancel.cancel();
            Ok::<_, anyhow::Error>(())
        };
        let client_task = async {
            let result = entry.connect(listener.local_addr()?, client).await;
            let _ = release_tx.send(());
            assert!(result.is_err(), "cancelled stage {stage} returned a session");
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(25), async {
            tokio::try_join!(server_task, cancel_task, client_task)
        }).await??;
    }
    Ok(())
}

#[tokio::test]
async fn late_ready_result_and_expired_temporary_result_are_rejected() -> Result<()> {
    let (lifetime, cancel) = Lifetime::new(Instant::now() + OFFER_LIFETIME);
    assert!(lifetime.run(async { cancel.cancel(); Ok(()) }).await.is_err());
    let (lifetime, _cancel) = Lifetime::new(Instant::now() + Duration::from_millis(5));
    assert!(lifetime.run(async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(())
    }).await.is_err());
    Ok(())
}
