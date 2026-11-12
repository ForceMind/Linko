//! Reserve one locally approved file for an existing v5 input lease.
//! This only correlates authenticated sessions. It grants no input and does
//! not start an OS drag. Use a separate connection on the layout sidecar ALPN.
use crate::{
    data,
    file_transfer::PreparedDropPath,
    layout_binding::Binding,
    layout_reservation::{require_end, require_first, verify_sidecar},
    verify_peer,
};
use anyhow::{ensure, Context, Result};
use peerglide_protocol::LEASE_MS;
use peerglide_transfer::drop_admission::{Admission, Context as DropContext, Ticket};
use quinn::{crypto::rustls::HandshakeData, Connection};
use std::{sync::Mutex, time::{Duration, Instant}};

const LABEL: &[u8] = b"EXPORTER-Linko-Verified-File-Drop-v1";
const IO_TIMEOUT: Duration = Duration::from_secs(4);
const MAGIC: &[u8; 4] = b"PGFD";
const BYTES: usize = 64;
const REQUEST: u8 = 1;
const READY: u8 = 2;
const DECLINED: u8 = 3;

/// Shared secret of a single authenticated file transfer. Never serialized.
#[derive(Clone)]
pub(crate) struct Receipt {
    transfer: [u8; 16],
    peer: blake3::Hash,
    key: [u8; 32],
}

impl Receipt {
    pub(crate) fn from_data(connection: &Connection, pin: &[u8], transfer: [u8; 16]) -> Result<Self> {
        ensure!(transfer != [0; 16] && connection.close_reason().is_none(), "file transfer ended");
        let handshake = connection
            .handshake_data()
            .context("file handshake unavailable")?
            .downcast::<HandshakeData>()
            .map_err(|_| anyhow::anyhow!("unexpected file handshake"))?;
        ensure!(handshake.protocol.as_deref() == Some(data::ALPN), "file data ALPN required");
        verify_peer(connection, pin)?;
        let mut key = [0; 32];
        connection
            .export_keying_material(&mut key, LABEL, &transfer)
            .map_err(|_| anyhow::anyhow!("file connection exporter unavailable"))?;
        Ok(Self { transfer, peer: blake3::hash(pin), key })
    }

    fn tag(&self, kind: u8, lease: u64, input: &Binding) -> blake3::Hash {
        let mut mac = blake3::Hasher::new_keyed(&self.key);
        mac.update(&[kind]);
        mac.update(&lease.to_le_bytes());
        mac.update(&self.transfer);
        mac.update(&input.proof());
        mac.finalize()
    }
}

/// The authenticated receiving peer reported this file committed. The sender
/// gives up this handle on one reservation attempt, including on failure.
pub struct SentReceipt(pub(crate) Receipt);

/// One locally armed file and at most one claim for it.
pub struct Slot(Mutex<Option<Prepared>>);

struct Prepared {
    context: DropContext,
    ticket: Ticket,
    expires: Instant,
    file: PreparedDropPath,
    receipt: Receipt,
    reservation: Option<Reservation>,
}

struct Reservation {
    lease: u64,
    input: Binding,
    expires: Instant,
}

/// Owns the prepared file after the exact live lease claims it. The local
/// adapter still checks this ticket when it submits or retires the drag.
pub struct Claim {
    pub file: PreparedDropPath,
    pub ticket: Ticket,
}

impl Slot {
    /// Called only by the local target controller after content and consent
    /// checks. The file handle leaves the original preparation on success.
    pub fn arm(
        mut file: PreparedDropPath,
        admission: &mut Admission,
        ticket: Ticket,
        context: DropContext,
        now: u64,
        authorized: bool,
    ) -> Result<Self> {
        ensure!(file.origin.connection() == context.connection, "prepared file connection differs");
        let receipt = file.receipt.take().context("prepared file has no transfer proof")?;
        let remaining = admission
            .file_ready(ticket, context, receipt.transfer, now, authorized)
            .map_err(|error| anyhow::anyhow!("file preparation unavailable ({error:?})"))?;
        Ok(Self(Mutex::new(Some(Prepared {
            context,
            ticket,
            expires: Instant::now() + Duration::from_millis(remaining),
            file,
            receipt,
            reservation: None,
        }))))
    }

    pub fn clear(&self) -> Result<()> {
        *self.lock()? = None;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<Prepared>>> {
        self.0.lock().map_err(|_| anyhow::anyhow!("file reservation slot unavailable"))
    }

    /// Only call from the local input owner after the normal v5 Acquire was
    /// granted. A stale context or withdrawn local approval retires the file.
    pub fn claim(
        &self,
        input: &Connection,
        pin: &[u8],
        granted_lease: u64,
        current: Option<DropContext>,
        admission: &mut Admission,
        now: u64,
    ) -> Result<Option<Claim>> {
        let mut state = self.lock()?;
        let Some(prepared) = live(&mut state, current) else {
            return Ok(None);
        };
        let Some(reservation) = &prepared.reservation else {
            return Ok(None);
        };
        if reservation.lease != granted_lease || prepared.receipt.peer != blake3::hash(pin)
            || !reservation.input.matches(input, pin, &reservation.input.proof())?
        {
            return Ok(None);
        }
        if admission
            .file_ready(prepared.ticket, prepared.context, prepared.receipt.transfer, now, true)
            .is_err()
        {
            *state = None;
            return Ok(None);
        }
        Ok(state.take().map(|prepared| Claim {
            file: prepared.file,
            ticket: prepared.ticket,
        }))
    }

    fn publish(
        &self,
        request: &Record,
        input: Binding,
        pin: &[u8],
        current: Option<DropContext>,
    ) -> Result<Option<Published<'_>>> {
        let mut state = self.lock()?;
        let Some(prepared) = live(&mut state, current) else {
            return Ok(None);
        };
        if prepared.reservation.is_some()
            || prepared.receipt.peer != blake3::hash(pin)
            || prepared.receipt.transfer != request.transfer
            || prepared.receipt.tag(REQUEST, request.lease, &input) != request.tag
        {
            return Ok(None);
        }
        let tag = prepared.receipt.tag(READY, request.lease, &input);
        prepared.reservation = Some(Reservation {
            lease: request.lease,
            input,
            expires: Instant::now() + Duration::from_millis(LEASE_MS),
        });
        Ok(Some(Published { slot: self, tag, confirmed: false }))
    }
}

fn live(state: &mut Option<Prepared>, current: Option<DropContext>) -> Option<&mut Prepared> {
    let now = Instant::now();
    if state.as_ref().is_some_and(|prepared| {
        Some(prepared.context) != current
            || now >= prepared.expires
            || prepared.reservation.as_ref().is_some_and(|grant| now >= grant.expires)
    }) {
        *state = None;
    }
    state.as_mut()
}

struct Published<'a> {
    slot: &'a Slot,
    tag: blake3::Hash,
    confirmed: bool,
}

impl Drop for Published<'_> {
    fn drop(&mut self) {
        if !self.confirmed {
            let _ = self.slot.clear();
        }
    }
}

/// Request correlation before the ordinary v5 Acquire. A positive reply
/// proves local preparation on the same connection and file, not input consent.
pub async fn reserve(
    sidecar: &Connection,
    input: &Connection,
    pin: &[u8],
    receipt: SentReceipt,
    lease: u64,
) -> Result<bool> {
    let receipt = receipt.0;
    verify_sidecar(sidecar, pin)?;
    ensure!(lease != 0 && receipt.peer == blake3::hash(pin), "file reservation peer or lease differs");
    let binding = Binding::from_v5(input, pin)?;
    let request = Record {
        kind: REQUEST,
        lease,
        transfer: receipt.transfer,
        tag: receipt.tag(REQUEST, lease, &binding),
    };
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.open_bi().await?;
        require_first(&send, &receive)?;
        send.write_all(&request.encode()).await?;
        send.finish()?;
        let mut bytes = [0; BYTES];
        receive.read_exact(&mut bytes).await?;
        require_end(&mut receive).await?;
        let reply = Record::decode(bytes)?;
        ensure!(reply.lease == lease && reply.transfer == receipt.transfer, "file reservation reply differs");
        match reply.kind {
            READY => {
                ensure!(reply.tag == receipt.tag(READY, lease, &binding), "file reservation proof differs");
                ensure!(binding.matches(input, pin, &binding.proof())?, "input binding retired");
                Ok(true)
            }
            DECLINED if reply.tag == blake3::Hash::from_bytes([0; 32]) => Ok(false),
            _ => anyhow::bail!("invalid file reservation reply"),
        }
    })
    .await
    .context("file reservation timed out")?
}

/// Check current local approval after receiving the bounded request. Publish
/// before acknowledging so the first normal v5 Acquire can find the slot.
pub async fn accept(
    sidecar: &Connection,
    input: &Connection,
    pin: &[u8],
    slot: &Slot,
    current: impl FnOnce() -> Option<DropContext>,
) -> Result<bool> {
    verify_sidecar(sidecar, pin)?;
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.accept_bi().await?;
        require_first(&send, &receive)?;
        let mut bytes = [0; BYTES];
        receive.read_exact(&mut bytes).await?;
        require_end(&mut receive).await?;
        let request = Record::decode(bytes)?;
        ensure!(request.kind == REQUEST, "invalid file reservation request");
        let binding = Binding::from_v5(input, pin)?;
        let published = slot.publish(&request, binding, pin, current())?;
        let (kind, tag) = published.as_ref().map_or((DECLINED, blake3::Hash::from_bytes([0; 32])),
                                                    |grant| (READY, grant.tag));
        let reply = Record { kind, tag, ..request };
        send.write_all(&reply.encode()).await?;
        send.finish()?;
        if let Some(mut published) = published {
            published.confirmed = true;
        }
        Ok(kind == READY)
    })
    .await
    .context("file reservation offer timed out")?
}

struct Record {
    kind: u8,
    lease: u64,
    transfer: [u8; 16],
    tag: blake3::Hash,
}

impl Record {
    fn encode(&self) -> [u8; BYTES] {
        let mut bytes = [0; BYTES];
        bytes[..4].copy_from_slice(MAGIC);
        bytes[4] = 1;
        bytes[5] = self.kind;
        bytes[8..16].copy_from_slice(&self.lease.to_le_bytes());
        bytes[16..32].copy_from_slice(&self.transfer);
        bytes[32..].copy_from_slice(self.tag.as_bytes());
        bytes
    }

    fn decode(bytes: [u8; BYTES]) -> Result<Self> {
        ensure!(&bytes[..4] == MAGIC && bytes[4] == 1 && bytes[6..8] == [0, 0], "invalid file reservation header");
        let lease = u64::from_le_bytes(bytes[8..16].try_into()?);
        let transfer = bytes[16..32].try_into()?;
        ensure!(lease != 0 && transfer != [0; 16], "invalid file reservation identity");
        Ok(Self { kind: bytes[5], lease, transfer, tag: blake3::Hash::from_bytes(bytes[32..].try_into()?) })
    }
}
