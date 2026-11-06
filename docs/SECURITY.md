# Security model

LAN peers are hostile until authenticated. Discovery grants no trust. TLS 1.3
mutually authenticates explicitly enrolled certificates, with post-handshake
leaf verification. No insecure verifier, shared default secret, automatic TOFU,
cloud relay or 0-RTT input. Fingerprints use BLAKE3; TLS uses rustls/ring.

`--allow-control` grants input for the session independently of certificate
trust. Clipboard/files are not implemented. Stop the session and remove/replace
the peer certificate to revoke CLI authorization. The GUI persists an explicitly confirmed fingerprint and input grant; revocation requires stopping the active session, then revoking the peer in Devices. Pairing codes and OS key-store protection are pending.

Frames are capped before allocation at 4096 bytes, input codes and motion ranges
validated, stream count/windows bounded. Input payloads do not implement Debug
and are not logged. No password/clipboard collection, telemetry or uploads.

Keys use exclusive file creation and Unix mode 0600. On Windows store identity
files under a user-private directory with normal user ACLs; hardened ACL/keychain
provisioning is pending. Never exchange private keys.

The GUI's local enrollment/revocation commit now prepares a bounded journal of
local settings and public certificates before replacing that pair. Only the profile owner
can recover it, and changed identity/files or unsupported records stop read-only.
Recovery never opens input/receiving permission. Pending records block ordinary
saves/reset; their checksum detects corruption, not network authentication. See
[ADR-0008](ADR/0008-recoverable-device-enrollment.md) for commit/durability and older
GUI limitations. Conflict repair and physical power-loss acceptance remain pending.

Identity certificate/private-key loads and GUI/CLI peer-certificate reads are
bounded to 16 KiB each before parsing (actual reads cap at limit + 1 to detect
growth). Non-regular, empty, oversized and malformed material rejects. Identity
loading requires a positive certificate/private-key SPKI match using rustls/ring;
it no longer treats merely readable files as a usable local identity.
GUI comparison confirmation is temporary and binds to the exact displayed
fingerprint. Enrollment reads again and rejects replacement contents even at the
same file path. Revoking peer trust also clears any pending confirmation. Parsing
a valid certificate never confers trust/input permission.

These file checks do not defend a hostile local filesystem/privileged process or
replace TLS validity/identity checks. Existing links to regular files can be read;
symlink-race hardening, OS keystore/Windows ACL provisioning, atomic multi-file
identity creation and key-memory zeroization remain pending. Failed identity
creation can leave partial exclusive files; they are never silently overwritten
or automatically deleted. Preserve existing identities during manual recovery.

Recovery uses callback-level escape/watchdog and receiver lease expiry with
tracked releases. OS-protected input is not bypassed. Process termination or OS
injection failure may prevent synthetic key release and needs physical testing.

Before stable release: independent security review, fuzzing/rate limiting, OS
key store, revocation UI, advisory/license review, signed/notarized packages.
Use private vulnerability reporting if available. Never put secrets or captured
input into public issues.


The GUI can exchange bounded versioned public connection information instead of
certificate files. It contains only a public DER certificate, input protocol
version and optional IPv4 endpoints. It is not an authentication token, pairing
code or grant. Imported endpoints and certificates remain untrusted until the
existing independent full-fingerprint comparison and explicit enrollment. The
parser rejects unknown fields/versions, oversized or malformed certificates and
addresses. Loading or editing it never saves trust or enables input; confirmation
cannot follow edited/unread text. Copying is explicit and never exports the key.
No input transport or ALPN changes, network enrollment or accept-any verifier are
introduced by this convenience format.


macOS prototypes now sign freshly assembled nested worker code and the complete
app with local ad-hoc integrity signatures, binding Info.plist and resource seals.
They have no Developer ID publisher identity or notarization. Default designated
requirements remain intact; fixed identifiers do not guarantee TCC continuity
between builds. No identifier-only requirements or private TCC entitlements are
used. Signing, strict validation and metadata failures prevent packaging; the
extracted ZIP must validate and modified plist/helper fixtures must fail. Existing
installed/artifact bytes, device keys, trust and OS permissions are never rewritten.

GUI diagnostics can separately query the actual sibling worker using only
`diagnose-context`, with bounded output, cancellation and a deadline. A diagnostic
snapshot grants no authority; native startup still checks the real permissions.
Launching a CLI from an automation host can have a different TCC responsible code
than normal app launch. Diagnose that attribution before asking users to expand
permissions. Do not reset TCC or disable OS protections as a workaround.
