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
