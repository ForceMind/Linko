# D03 offline receive manager

manager::Manager connects the existing real-file Receiver to independent
FileReceive consent, expected authenticated peer binding and an explicit local
per-offer accept/resume call. No network or GUI calls it yet.

- At most four active receivers, 128 admitted IDs/history entries, and a caller
  selected cumulative byte admission budget. Failed admissions, cancelled
  transfers and committed files keep their reservation and ID. Zero-byte offers
  still consume a bounded history slot. IDs cannot be reused within this manager.
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

This is a per-manager lifetime admission bound, NOT a filesystem free-space,
process-global or persistent disk quota. Recreating managers/restarting can
accumulate retained files. Product integration must inventory old staging data,
coordinate budgets across peers/processes, query free space, provide explicit
inspection/cleanup and persist history before enabling network admission.
A caller must never infer a fresh budget merely from reconnect. Hostile local FS
races, disk-full/power-loss, native storage media and physical throughput remain
separate acceptance. No file/clipboard input grant inheritance or input v5 change.
