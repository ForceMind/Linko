# Cloud-tested local foundations

This increment does not enable native gestures or execute actions. It prepares
deterministic logic and working GUI configuration for local-machine integration.

## Ownership model (G03)

`peerglide-core::gesture_broker` freezes target/focus/config/trigger context and
tracks Idle, BypassHeld, Pending, Tracking, CancelledHeld. A passed physical down
keeps its passed up even if enabled mid-hold. A suppressed down keeps ownership
of its eventual up through cancellation, without holding movement or other keys.
Short clicks emit one balanced-click *intent*, never a raw orphan up. Default
movement threshold is 12 logical pixels; a stationary hold over 400 ms cancels
instead of pretending it was a short click. Tracking rejection never falls back
to an arbitrary click or drag replay. Injected events cannot start/finish candidates.

The model requests edge inhibition while pending/tracking and interior rearming
after completion/cancellation. Remote-control mode, exclusion/permission changes,
context change, clock regression, timeout and emergency stop cancel. Consuming
tickets recheck context, consent/local target, issuance time and exact expiry.

This is a worker/reference model, not a native callback implementation: candidate
allocation and recognition MUST NOT run in a global hook. Local Codex must connect
the synchronous ownership decision and bounded queue to the existing single
capture lifecycle, provide authentic native context, retain the button ledger,
tag replay as ActionInjected and check OS submission/recovery. No hook was added.
The model does not make OS focus checks atomic or promise exactly-once OS effects.

## Rules and GUI (G01/G04)

`peerglide-actions::rules` has strict schema-1 parameter-free actions, 64 KiB
decode bound, at most 128 rules/exclusions and 32 exact app IDs per group. Safety
disable/exclusions precede exact application, group and global scope. Priority
breaks ties only within a scope rank; equal-rank/priority overlaps reject at
compile, independent of file order. No window-title guessing, wildcard, command,
script, path execution or conversion from KVM grants exists.

Gesture rules can be added/removed, scoped, mapped and saved in the GUI. Lab
preview uses an explicit application ID and shows a selected semantic action;
it does not dispatch it. Saving rules keeps the global-enable flag false. Native
app-ID discovery and global input consent remain separate integration work.

## Versioned persistence and recovery

GUI settings/rules use store envelope 1, bounded reads, strict schemas, unique
same-directory temporary files, sync-before-rename and exact previous-byte backups
named by BLAKE3. Legacy flat settings load without silently enabling new features;
the first save preserves the original bytes before migration. Corrupt/future or
oversized files are not overwritten with defaults. Explicit backup/reset controls
recover bounded files; reset disables grants and preserves identity files.
Oversized/unreadable files need manual recovery rather than an unbounded backup.

Cooperating writers use a stable OS-locked sidecar and compare the original hash
before commit; stale snapshots fail visibly. GUI instances hold a folder-level
lock so a second GUI cannot start workers or write settings/trust. Lock guards
explicitly unlock on drop, including when a duplicated/inherited descriptor still
exists; the regression covers this case without retrying failed assertions.
See [Rust File locking](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
GUI Rust minimum is 1.89. No stale-lock-file deletion is needed.

Locks coordinate these GUI versions, not hostile local writers or manual CLI
processes. Settings + certificate enrollment are not one filesystem transaction.
Windows power-loss durability, OS keychain/ACL, live revocation across independent
CLI agents and cross-process native capture exclusion still need separate work.
Replacing the enrolled peer now clears its inherited input grant.

## Verification

Local fmt, strict workspace Clippy, 90 workspace tests, Debug build and real
managed/reconnect process smokes pass. Added coverage includes 1,024 mode/ownership
transitions, stale/revoked/expired tickets, rule specificity/conflicts, exact legacy
backups, corrupt/future files, stale writers, duplicated lock handles, peer grant
reset and second-GUI exclusion. Native matrix and Linux UI smoke remain separate
candidate gates. #3 physical acceptance and #10 historical root cause remain open.
