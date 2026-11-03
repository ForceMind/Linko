# D03 offline transfer storage foundation

`peerglide-transfer` is independently runnable through its Rust integration/unit
scenarios. It is not wired to QUIC, input queues, GUI, clipboard or OS drag/drop.
No network/file permission is granted by constructing a PeerBinding. Future callers
must bind it to the independently authenticated TLS fingerprint, cryptographically
random transfer ID and current explicit per-direction file grant, plus revocation,
acceptance and quotas. Do not copy fixture bindings into production.

## Implemented contract

- One regular file, Unicode portable basename (no path, DOS devices, bidi controls,
  leading/trailing whitespace, dot names, ADS, separators). No directory/symlink offers.
- Fixed 256 KiB chunks, maximum 8 GiB/file, maximum 5 MiB serialized manifest.
  Before serde, `decode` bounds input; a network adapter must also bound allocation
  before receiving bytes. Strict version/schema, exact chunk count and length.
- BLAKE3 per-chunk and whole-file digests. Sender freezes hashes and rechecks each
  source chunk before handing it to transport; this is not an atomic snapshot of
  a concurrently edited source. Mid-transfer changes reject rather than mutate
  an accepted offer. No content/name logging or automatic execution.
- Receiver uses an explicitly selected existing directory, opaque digest-derived
  private staging directory and exclusive files. Unix staging 0700/data 0600;
  Windows relies on the explicitly selected user-private directory's ACLs.
- Resume binds the expected sender identity, transfer ID and exact manifest.
  It scans disk and rehashes each chunk instead of trusting progress counters.
  Matching zero-filled preallocation may count as verified zero-content chunks.
- Out-of-order chunks and valid duplicates work; bad lengths/hashes never advance
  progress. An OS file lock rejects concurrent cooperating receivers. Acknowledged
  chunks are synced; incomplete/corrupt file cannot publish.
- Commit rehashes disk and adds a hard link to the final basename without overwrite.
  Existing files/symlinks are never replaced. Hard-link-unsupported filesystems
  fail; there is no overwrite-rename fallback. No automatic opening/launching.
- A durable terminal marker precedes publication. Resume rejects that marker even
  after a final file was moved: it must never modify an inode already published by
  hard link. Partial is unlinked after publication; receipt/marker remain as local
  recovery data. Errors after publication have an unknown outcome: inspect the
  destination manually, never auto-replay. No automatic cleanup of user data.

## Verification and limits

Tests perform real temporary-file transfer/resume, Unicode names, reordering,
negative chunk/whole hashes, post-write disk corruption, binding/receipt changes,
cooperating locks, crash-after-link/moved-inode protection, empty files, destination
collisions, source mutations, max-size manifest and Unix symlink rejection.

Native matrix tests ordinary runner filesystems, not USB/exFAT, power-loss durability,
real 8 GiB throughput, hostile local filesystem races or volume/network mount behavior.
Ancestor symlinks/malicious privileged processes are not defended. Windows ACL
provisioning and native OS links need separate acceptance. No cancel/consent UI,
aggregate disk quota, bounded transfer concurrency/history/cleanup, streaming protocol,
backpressure, disk-full fault injection or remote authorization path is implemented.
Whole-file resume scanning is O(file size); no 8 GiB allocation is needed.

These are D03 building blocks, not completed M4 or a native drag implementation.
See existing #6 and LOCAL_CODEX_HANDOFF.zh-CN.md before product integration.
