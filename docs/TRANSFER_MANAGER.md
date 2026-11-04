# D03 offline receive manager

manager::Manager connects the existing real-file Receiver to independent
FileReceive consent, expected authenticated peer binding and an explicit local
per-offer accept/resume call. No network or GUI calls it yet.

- At most four active receivers, 128 admitted IDs/history entries, and a caller
  selected cumulative byte admission budget. Failed admissions, cancelled
  transfers and committed files keep their reservation and ID. Zero-byte offers
  still consume a bounded history slot. Runtime IDs cannot be reused within this manager;
  persistent IDs are scoped to the authenticated peer and cannot be repurposed.
- Permission and peer checks precede filesystem admission. Each chunk checks its
  original local grant ticket, so replacing/revoking consent cannot revive queued
  work. Consent replacement, revoke and close release active handles.
- Cancellation retains partial data for explicit recovery; no recursive cleanup,
  user-file deletion, automatic resume or automatic retry.
- Commit checks fresh monotonic time before verification and again before terminal
  marker/publication. Permission expiry during hashing/sync prevents publication.
  Any uncertain commit failure remains UnknownOutcome and cannot be replayed.
  Existing non-overwrite/full-hash/terminal-marker guarantees remain authoritative.
- Summary/history contains state and byte counters only. No file contents, basename,
  destination, peer identity or content hash is logged by this layer.
- Tests use real temporary files for admission, permission denial, wrong binding,
  cumulative/active limits, cancellation, resume, final content, revocation and
  consent expiry between hash and publication.

## Durable receive-root accounting

Opening a Manager now holds a cooperating OS lock for the selected receive root,
including while its session is closed. Drop closes receivers before releasing the
root lock. Independent Managers cannot race a shared root, even for different peers.
The low-level Receiver remains an offline primitive: callers must not use it to
write a root owned by a Manager. Other applications and hostile local writers are
not controlled by this lock.

Before staging a new offer, Manager exclusively creates and syncs a version-1
reservation with the exact peer/transfer binding, manifest digest, receipt digest
and logical byte count. It then syncs the directory on Unix before creating the
Receiver. Failed/uncertain reservation I/O poisons new admissions for that owner;
failed Receiver creation keeps the durable reservation. No automatic refunds,
record overwrites, cleanup, deletion, retry or grant restoration occur.

Startup bounds the scan to 4096 root entries and 128 reservations, each at most
2048 bytes. Admissions and publication recheck entry capacity so their own new
files cannot push the root past its reopenable limit. Existing receiver directories are validated against bounded receipts,
canonical directory binding, regular files, partial size and terminal markers.
Old valid receipts are imported only after validating the full inventory. Malformed,
future-version, conflicting, oversized, symlink or incomplete initialization data
rejects opening and preserves originals for explicit inspection. Partial legacy
migration is conservative and idempotent on reopening.

`admitted_bytes` includes all durable reservations across reconnects/restarts,
including cancelled, failed and published files, even if the published file was
moved or deleted. An exact explicit resume reuses its existing reservation without
double charging; it still requires a fresh session and independent receive consent.
A budget below existing usage admits nothing, including resume, until explicitly
increased. Changing an existing peer/transfer ID's manifest rejects.

`inventory()` exposes aggregate reserved bytes/count and startup partial/terminal
counts only. A terminal marker means **unknown publication outcome**, not a verified
committed history entry. Per-transfer status/history remains runtime-only; durable
user-facing history, inspect/cleanup UI and explicit quota reclamation remain work.
No content, filenames, destinations or identities are logged or uploaded.

This is a conservative cumulative admission quota for one root, not an OS disk
free-space check, a limit across different roots, or physical allocated-block
accounting. Metadata overhead is bounded separately. Windows directory power-loss
durability, disk-full/power-loss, hostile local FS races, native media and physical
throughput remain separate acceptance. Network/GUI integration stays disabled;
input v5 and independent file/clipboard permissions are unchanged.
