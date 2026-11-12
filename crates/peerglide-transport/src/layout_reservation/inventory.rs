//! Opt-in display geometry on the authenticated layout connection.
use super::{require_end, require_first, verify_sidecar, Binding, IO_TIMEOUT};
use anyhow::{ensure, Context, Result};
use quinn::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAGIC: &[u8; 4] = b"PGI1";
const QUERY_SIZE: usize = 36;
const MAX_BYTES: usize = 16 * 1024;
const MAX_DISPLAYS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// Session-local ordinal, separate from the native OS identifier.
    pub id: u8,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    pub quarter_turns: u8,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub generation: u64,
    pub displays: Vec<Display>,
}

impl Inventory {
    fn validate(&self) -> Result<()> {
        ensure!(self.generation != 0 && (1..=MAX_DISPLAYS).contains(&self.displays.len()),
            "display inventory generation or size invalid");
        ensure!(self.displays.iter().filter(|display| display.primary).count() == 1,
            "display inventory must have one primary");
        let mut ids = BTreeSet::new();
        for display in &self.displays {
            ensure!(
                (1..=MAX_DISPLAYS as u8).contains(&display.id)
                    && ids.insert(display.id)
                    && display.width != 0
                    && display.height != 0
                    && display.left.checked_add_unsigned(display.width).is_some()
                    && display.top.checked_add_unsigned(display.height).is_some()
                    && display.scale_milli != 0
                    && display.quarter_turns < 4,
                "display inventory contains invalid geometry or duplicate ordinals"
            );
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() <= MAX_BYTES, "display inventory exceeds byte limit");
        Ok(bytes)
    }
}

/// A separate short layout connection is consumed by this query. It uses the
/// same explicit pin and the exporter from the live v5 input connection.
pub async fn query(sidecar: &Connection, input: &Connection, pin: &[u8]) -> Result<Option<Inventory>> {
    verify_sidecar(sidecar, pin)?;
    let binding = Binding::from_v5(input, pin)?;
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.open_bi().await?;
        require_first(&send, &receive)?;
        let mut request = [0; QUERY_SIZE];
        request[..4].copy_from_slice(MAGIC);
        request[4..].copy_from_slice(&binding.proof());
        send.write_all(&request).await?;
        send.finish()?;
        let mut header = [0; 6];
        receive.read_exact(&mut header).await?;
        ensure!(&header[..4] == MAGIC, "display inventory header invalid");
        let length = usize::from(u16::from_le_bytes(header[4..].try_into()?));
        ensure!(length <= MAX_BYTES, "display inventory response too large");
        if length == 0 {
            require_end(&mut receive).await?;
            return Ok(None);
        }
        let mut bytes = vec![0; length];
        receive.read_exact(&mut bytes).await?;
        require_end(&mut receive).await?;
        let inventory: Inventory = serde_json::from_slice(&bytes)?;
        inventory.validate()?;
        ensure!(binding.matches(input, pin, &binding.proof())?, "display inventory input connection changed");
        Ok(Some(inventory))
    })
    .await
    .context("display inventory query timed out")?
}

/// Only the trusted local controller can opt in to disclose its current
/// geometry. Replying with geometry never authorizes a native input lease.
pub async fn answer(
    sidecar: &Connection,
    input: &Connection,
    pin: &[u8],
    choose: impl FnOnce() -> Option<Inventory>,
) -> Result<bool> {
    verify_sidecar(sidecar, pin)?;
    let binding = Binding::from_v5(input, pin)?;
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.accept_bi().await?;
        require_first(&send, &receive)?;
        let mut request = [0; QUERY_SIZE];
        receive.read_exact(&mut request).await?;
        require_end(&mut receive).await?;
        ensure!(&request[..4] == MAGIC, "display inventory request invalid");
        let proof = request[4..].try_into()?;
        let bytes = if binding.matches(input, pin, &proof)? {
            choose().map(|inventory| inventory.encode()).transpose()?
        } else {
            None
        };
        let mut reply = [0; 6];
        reply[..4].copy_from_slice(MAGIC);
        let length = bytes.as_ref().map_or(0, Vec::len);
        reply[4..].copy_from_slice(&u16::try_from(length)?.to_le_bytes());
        send.write_all(&reply).await?;
        if let Some(bytes) = bytes {
            send.write_all(&bytes).await?;
        }
        send.finish()?;
        Ok(length != 0)
    })
    .await
    .context("display inventory offer timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::endpoint;
    use crate::Identity;
    use quinn::Endpoint;
    use std::time::Duration;

    fn geometry() -> Inventory {
        Inventory {
            generation: 4,
            displays: vec![Display {
                id: 1,
                left: -900,
                top: 100,
                width: 900,
                height: 1600,
                scale_milli: 1750,
                quarter_turns: 1,
                primary: true,
            }],
        }
    }

    #[test]
    fn geometry_requires_unique_ordinals_and_bounded_rectangles() -> Result<()> {
        let valid = geometry();
        assert_eq!(serde_json::from_slice::<Inventory>(&valid.encode()?)?, valid);
        let mut duplicate = valid.clone();
        let mut screen = duplicate.displays[0].clone();
        screen.primary = false;
        duplicate.displays.push(screen);
        assert!(duplicate.encode().is_err());
        duplicate.displays[1].id = 2;
        duplicate.displays[1].left = i32::MAX;
        assert!(duplicate.encode().is_err());
        duplicate.displays.resize(MAX_DISPLAYS + 1, valid.displays[0].clone());
        assert!(duplicate.encode().is_err());
        Ok(())
    }

    async fn pair(client: &Endpoint, server: &Endpoint) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let connecting = client.connect(server.local_addr()?, "peerglide.local")?;
            let accepting = async {
                let incoming = server.accept().await.context("missing display peer")?;
                Ok::<_, anyhow::Error>(incoming.await?)
            };
            let (client, server) = tokio::join!(connecting, accepting);
            Ok((client?, server?))
        }).await?
    }

    #[tokio::test]
    async fn only_the_bound_input_connection_can_query_a_locally_approved_inventory() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let input_a = crate::endpoint(bind, &a, &b.certificate)?;
        let input_b = crate::endpoint(bind, &b, &a.certificate)?;
        let sidecar_a = endpoint(bind, &a, &b.certificate)?;
        let sidecar_b = endpoint(bind, &b, &a.certificate)?;
        let (client, server) = pair(&input_a, &input_b).await?;
        let (_other_client, wrong) = pair(&input_a, &input_b).await?;
        let (offer, answerer) = pair(&sidecar_a, &sidecar_b).await?;
        let (received, sent) = tokio::try_join!(
            query(&offer, &client, &b.certificate),
            answer(&answerer, &wrong, &a.certificate, || panic!("wrong input proof")),
        )?;
        assert!(!sent && received.is_none());
        let (offer, answerer) = pair(&sidecar_a, &sidecar_b).await?;
        let (received, sent) = tokio::try_join!(
            query(&offer, &client, &b.certificate),
            answer(&answerer, &server, &a.certificate, || Some(geometry())),
        )?;
        assert!(sent);
        assert_eq!(received, Some(geometry()));
        Ok(())
    }
}
