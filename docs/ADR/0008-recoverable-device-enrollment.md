# ADR-0008: Recoverable local device enrollment

Status: implementing; automatic fault/restart checks pending.

## Problem

Manual enrollment wrote trusted-peer.crt.der before saving settings.json. A stale
settings snapshot or write failure could therefore replace the old certificate
without completing the new registration. Revocation had the same two-file gap.
These paths must be reliable before asynchronous authenticated pairing is added.

## Decision

Keep the existing public certificate/fingerprint files and input v5. Use the GUI
profile owner lock and hold the settings writer lock across preparation and the
two replacements. Reject stale settings or a certificate inconsistent with the
saved fingerprint before modifying primary files. Preserve hash-named exact
backups; an existing conflicting backup is never overwritten.

Persist an exclusive enrollment-pending.json before changing either primary file.
It contains bounded old/new settings, public certificates and the local public
identity snapshot, never a private key. Its version and checksum detect unsupported
formats and accidental corruption; the checksum is not authentication. Paths are
fixed by code, not loaded from this record. Settings remain at 64 KiB, certificates
at 16 KiB each, and the whole journal at 1 MiB including JSON encoding.

The durable intent is the local commit boundary. After it, interrupted writes need
recovery rather than being reported as cancelled or completed. Enrollment consumes
the manual fingerprint confirmation and clears native/receiving permission; the
existing separate permission step may then obtain fresh consent. Revocation clears
in-memory grants and requests Stop even if persistence fails.

On restart, only the GUI owning that profile may finish the recorded intent. Both
primary files must still equal their recorded old or new bytes and the local public
identity must match. Recovery rolls forward, including interrupted revocation; it
never restores a revoked old grant. File/identity changes, invalid/future records,
or inconsistent grants leave the files intact and the GUI read-only. Ordinary save
and explicit settings reset cannot bypass a pending record. No network session or
input is started by recovery. It is idempotent after successful journal removal.

This is recoverable two-file commit, not one filesystem-atomic operation or proof
against a hostile local process/filesystem. Per-file writes use exclusive staging,
sync and replacement; Unix directory sync is retained, while equivalent Windows
power-loss durability remains unproven. A failed/partial journal write is preserved
read-only, not silently discarded. Conflict-resolution UI is future work. Older
GUIs do not understand this journal: finish recovery with this version before
opening an older GUI on a profile with an interrupted enrollment.

## Validation and boundaries

Test actual helper-process death after intent sync, certificate replacement and
settings replacement, for both enrollment and revocation. Reopen the real App model
and verify consistent files, closed permissions and unchanged identity/key bytes.
Cover stale/future settings, changed peer/local identity, checksum/grant tampering,
oversized/invalid records, second GUI ownership and backup collisions. These are
disk/process tests, not physical C06 or a complete security audit.

The caller is still the explicitly confirmed manual enrollment UI. No short code,
PAKE verifier, fixture authorization, TLS exception, new input hook, data grant,
OS permission or protocol migration is introduced. A later PAKE path needs its own
reviewed authentication/commit receipt, not a fabricated manual confirmation.
