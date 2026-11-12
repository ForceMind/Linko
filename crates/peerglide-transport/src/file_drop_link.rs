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
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const LABEL: &[u8] = b"EXPORTER-Linko-Verified-File-Drop-v1";
const IO_TIMEOUT: Duration = Duration::from_secs(4);
const MAGIC: &[u8; 4] = b"PGFD";
const BYTES: usize = 64;
const REQUEST: u8 = 1;
const READY: u8 = 2;
const DECLINED: u8 = 3;

/// Purpose-scoped proof of one authenticated file transfer. Export is limited
/// to the local worker pipe and never logged, persisted, or sent to a peer.
#[derive(Clone)]
pub(crate) struct Receipt {
    transfer: [u8; 16],
    peer: blake3::Hash,
    key: [u8; 32],
}

impl Receipt {
    pub(crate) fn from_data(
        connection: &Connection,
        pin: &[u8],
        transfer: [u8; 16],
    ) -> Result<Self> {
        ensure!(
            transfer != [0; 16] && connection.close_reason().is_none(),
            "file transfer ended"
        );
        let handshake = connection
            .handshake_data()
            .context("file handshake unavailable")?
            .downcast::<HandshakeData>()
            .map_err(|_| anyhow::anyhow!("unexpected file handshake"))?;
        ensure!(
            handshake.protocol.as_deref() == Some(data::ALPN),
            "file data ALPN required"
        );
        verify_peer(connection, pin)?;
        let mut key = [0; 32];
        connection
            .export_keying_material(&mut key, LABEL, &transfer)
            .map_err(|_| anyhow::anyhow!("file connection exporter unavailable"))?;
        Ok(Self {
            transfer,
            peer: blake3::hash(pin),
            key,
        })
    }

    fn tag(&self, kind: u8, lease: u64, input: &Binding) -> blake3::Hash {
        let mut mac = blake3::Hasher::new_keyed(&self.key);
        mac.update(&[kind]);
        mac.update(&lease.to_le_bytes());
        mac.update(&self.transfer);
        mac.update(&input.proof());
        mac.finalize()
    }

    fn worker_token(&self) -> String {
        let mut token = String::with_capacity(peerglide_protocol::file_drop_ipc::PROOF_HEX);
        for byte in self.transfer {
            write!(&mut token, "{byte:02x}").expect("format into String");
        }
        token.push_str(self.peer.to_hex().as_str());
        token.push_str(blake3::Hash::from_bytes(self.key).to_hex().as_str());
        token
    }

    fn from_worker_token(token: &str) -> Result<Self> {
        ensure!(
            token.len() == peerglide_protocol::file_drop_ipc::PROOF_HEX && token.is_ascii(),
            "invalid local file reservation proof"
        );
        let mut transfer = [0; 16];
        for (index, byte) in transfer.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&token[offset..offset + 2], 16)?;
        }
        let peer = blake3::Hash::from_hex(&token[32..96])?;
        let key = *blake3::Hash::from_hex(&token[96..160])?.as_bytes();
        ensure!(transfer != [0; 16] && key != [0; 32], "invalid local file reservation proof");
        Ok(Self { transfer, peer, key })
    }
}

/// The authenticated receiving peer reported this file committed. The sender
/// gives up this handle on one reservation attempt, including on failure.
pub struct SentReceipt(pub(crate) Receipt);

pub(crate) struct ReceivedReceipt {
    pub(crate) binding: Receipt,
    pub(crate) connection: [u8; 32],
}

/// One locally armed file and at most one claim for it.
pub struct Slot(Reservations<LocalFile>);

struct LocalFile {
    context: DropContext,
    ticket: Ticket,
    file: PreparedDropPath,
}

/// The input worker owns only the short-lived proof, never the local file.
pub struct WorkerSlot(Reservations<()>);

struct Reservations<P>(Mutex<Option<Prepared<P>>>);

struct Prepared<P> {
    local: P,
    expires: Instant,
    receipt: Receipt,
    reservation: Option<Reservation>,
}

struct Reservation {
    lease: u64,
    input: Binding,
    expires: Instant,
}

/// Owns the prepared file. The local adapter still checks its ticket when it
/// submits or retires the drag.
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
        let receipt = file
            .receipt
            .take()
            .context("prepared file has no transfer proof")?;
        ensure!(
            receipt.connection == context.connection,
            "prepared file connection differs"
        );
        let remaining = admission
            .file_ready(ticket, context, receipt.binding.transfer, now, authorized)
            .map_err(|error| anyhow::anyhow!("file preparation unavailable ({error:?})"))?;
        Ok(Self(Reservations::new(
            LocalFile { context, ticket, file },
            receipt.binding.clone(),
            Duration::from_millis(remaining),
        )))
    }

    pub fn clear(&self) -> Result<()> {
        self.0.clear()
    }

    /// Separate the GUI-owned file from the proof for one managed input worker.
    /// The token is only for inherited stdin and must never enter diagnostics.
    pub fn into_worker(self) -> Result<(Claim, String, u32)> {
        let prepared = self.0.0.into_inner().ok().flatten().context("file preparation ended")?;
        let remaining = prepared.expires.saturating_duration_since(Instant::now());
        let lifetime_ms = u32::try_from(remaining.as_millis())?;
        ensure!(prepared.reservation.is_none() && lifetime_ms != 0, "file preparation unavailable");
        Ok((Claim { file: prepared.local.file, ticket: prepared.local.ticket }, prepared.receipt.worker_token(), lifetime_ms))
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
        Ok(self.0.claim(input, pin, granted_lease, |prepared| {
            Some(prepared.local.context) == current
                && admission.file_ready(
                    prepared.local.ticket,
                    prepared.local.context,
                    prepared.receipt.transfer,
                    now,
                    true,
                ).is_ok()
        })?.map(|local| Claim { file: local.file, ticket: local.ticket }))
    }
}

impl WorkerSlot {
    /// Accept only a bounded proof supplied by this worker's local supervisor.
    pub fn from_local_arm(proof: &str, lifetime_ms: u32, pin: &[u8]) -> Result<Self> {
        ensure!((1..=peerglide_protocol::file_drop_ipc::MAX_LIFETIME_MS).contains(&lifetime_ms), "file reservation lifetime invalid");
        let receipt = Receipt::from_worker_token(proof)?;
        ensure!(receipt.peer == blake3::hash(pin), "file reservation peer differs");
        Ok(Self(Reservations::new(
            (),
            receipt,
            Duration::from_millis(u64::from(lifetime_ms)),
        )))
    }

    pub fn clear(&self) -> Result<()> {
        self.0.clear()
    }

    /// Report only an already-granted input lease, once.
    pub fn claim(&self, input: &Connection, pin: &[u8], granted_lease: u64, current: bool) -> Result<bool> {
        Ok(self.0.claim(input, pin, granted_lease, |_| current)?.is_some())
    }

    /// Denial of the exact lease retires its file proof without granting it.
    pub fn retire_denied(&self, input: &Connection, pin: &[u8], lease: u64, current: bool) -> Result<bool> {
        Ok(self.0.claim(input, pin, lease, |_| current)?.is_some())
    }
}

impl<P> Reservations<P> {
    fn new(local: P, receipt: Receipt, lifetime: Duration) -> Self {
        Self(Mutex::new(Some(Prepared {
            local,
            expires: Instant::now() + lifetime,
            receipt,
            reservation: None,
        })))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<Prepared<P>>>> {
        self.0.lock().map_err(|_| anyhow::anyhow!("file reservation slot unavailable"))
    }

    fn clear(&self) -> Result<()> {
        *self.lock()? = None;
        Ok(())
    }

    fn claim(
        &self,
        input: &Connection,
        pin: &[u8],
        granted_lease: u64,
        current: impl FnOnce(&Prepared<P>) -> bool,
    ) -> Result<Option<P>> {
        let mut state = self.lock()?;
        let Some(prepared) = live(&mut state, current) else {
            return Ok(None);
        };
        let Some(reservation) = &prepared.reservation else {
            return Ok(None);
        };
        if reservation.lease != granted_lease
            || prepared.receipt.peer != blake3::hash(pin)
            || !reservation.input.matches(input, pin, &reservation.input.proof())?
        {
            return Ok(None);
        }
        Ok(state.take().map(|prepared| prepared.local))
    }

    fn publish(
        &self,
        request: &Record,
        input: Binding,
        pin: &[u8],
        current: impl FnOnce(&Prepared<P>) -> bool,
    ) -> Result<Option<Published<'_, P>>> {
        let mut state = self.lock()?;
        let Some(prepared) = live(&mut state, current) else {
            return Ok(None);
        };
        let receipt = &prepared.receipt;
        if prepared.reservation.is_some()
            || receipt.peer != blake3::hash(pin)
            || receipt.transfer != request.transfer
            || receipt.tag(REQUEST, request.lease, &input) != request.tag
        {
            return Ok(None);
        }
        let tag = receipt.tag(READY, request.lease, &input);
        prepared.reservation = Some(Reservation {
            lease: request.lease,
            input,
            expires: Instant::now() + Duration::from_millis(LEASE_MS),
        });
        Ok(Some(Published { slot: self, tag, confirmed: false }))
    }
}

fn live<P>(state: &mut Option<Prepared<P>>, current: impl FnOnce(&Prepared<P>) -> bool) -> Option<&mut Prepared<P>> {
    let now = Instant::now();
    if state.as_ref().is_some_and(|prepared| {
        now >= prepared.expires
            || prepared.reservation.as_ref().is_some_and(|grant| now >= grant.expires)
            || !current(prepared)
    }) {
        *state = None;
    }
    state.as_mut()
}

struct Published<'a, P> {
    slot: &'a Reservations<P>,
    tag: blake3::Hash,
    confirmed: bool,
}

impl<P> Drop for Published<'_, P> {
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
    ensure!(
        lease != 0 && receipt.peer == blake3::hash(pin),
        "file reservation peer or lease differs"
    );
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
        ensure!(
            reply.lease == lease && reply.transfer == receipt.transfer,
            "file reservation reply differs"
        );
        match reply.kind {
            READY => {
                ensure!(
                    reply.tag == receipt.tag(READY, lease, &binding),
                    "file reservation proof differs"
                );
                ensure!(
                    binding.matches(input, pin, &binding.proof())?,
                    "input binding retired"
                );
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
    accept_inner(sidecar, input, pin, &slot.0, |prepared| current() == Some(prepared.local.context)).await
}

pub async fn accept_worker(
    sidecar: &Connection,
    input: &Connection,
    pin: &[u8],
    slot: &WorkerSlot,
    current: impl FnOnce() -> bool,
) -> Result<bool> {
    accept_inner(sidecar, input, pin, &slot.0, |_| current()).await
}

async fn accept_inner<P>(
    sidecar: &Connection,
    input: &Connection,
    pin: &[u8],
    slot: &Reservations<P>,
    current: impl FnOnce(&Prepared<P>) -> bool,
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
        let published = slot.publish(&request, binding, pin, current)?;
        let (kind, tag) = published
            .as_ref()
            .map_or((DECLINED, blake3::Hash::from_bytes([0; 32])), |grant| {
                (READY, grant.tag)
            });
        let reply = Record {
            kind,
            tag,
            ..request
        };
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
        ensure!(
            &bytes[..4] == MAGIC && bytes[4] == 1 && bytes[6..8] == [0, 0],
            "invalid file reservation header"
        );
        let lease = u64::from_le_bytes(bytes[8..16].try_into()?);
        let transfer = bytes[16..32].try_into()?;
        ensure!(
            lease != 0 && transfer != [0; 16],
            "invalid file reservation identity"
        );
        Ok(Self {
            kind: bytes[5],
            lease,
            transfer,
            tag: blake3::Hash::from_bytes(bytes[32..].try_into()?),
        })
    }
}
