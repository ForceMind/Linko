# Opt-in mDNS hints (P02 slice)

Devices → Find LAN computers starts browsing `_peerglide._udp.local.`. It does
not publish a machine identity, enroll a certificate, connect, grant control or
enable native input. A separate button advertises an already running GUI listener
on port 24870. Only a public certificate fingerprint/protocol version and generic
fingerprint-derived name/host are published, never the OS hostname/user/private key.
Discovery is off at every launch. Existing manual address/enrollment remains usable.

All results are `UntrustedHint`. Names/TXT may be spoofed. Use address only fills
the next connection address; the pinned certificate and its separate grant are
unchanged. A different certificate still requires independent comparison/enrollment,
which clears the old peer's grant. Hints do not perform short-code authentication
or automatic trust. A hint does not mean an authenticated/connected computer.

## Bounds and lifecycle

- Version 5, nonzero port, 64 ASCII hex fingerprint, control-free name <=255 bytes.
- First slice selects IPv4 private/link-local addresses, up to 4 per record.
  IPv6, routed public/Tailscale addresses and multi-interface acceptance are pending.
- At most 64 displayed records, expiry at 90 seconds, refresh subscription every
  30 seconds; removed records disappear. Poll consumes at most 128 events/channel.
- Explicit nonblocking stop has an acknowledgement; a new daemon cannot start
  until the old stop completes. Listener exit withdraws its advertisement.
  UDP loss/caches mean withdrawal is not an instantaneous privacy/availability guarantee.
- Async daemon errors and a disconnected subscription clear hints and display
  retry/manual-address guidance. UI stalls do not turn hints into authoritative state.

The own-layer table/event limits are NOT a proof that the third-party daemon's
entire DNS cache is globally memory-bounded. Hostile-LAN flood/resource/packet
testing and independent security review remain release gates; do not advertise a
stable/DoS-proof release from these unit tests. The daemon is separate from the
native capture lifecycle; no OS input hook or permission is added by browsing.

## Dependency / platform evidence

Uses `mdns-sd` exactly 0.21.7 with default features disabled (no library logging or
async runtime dependency). Cargo metadata reports Apache-2.0 OR MIT, Rust 1.71.
PeerGlide GUI still requires at least Rust 1.95. Existing notice collection includes
the crate and its dependencies; allowed-expression screening passes for 354 locked
dependencies. This is not an independent license or security audit.

Primary API reference: https://docs.rs/mdns-sd/0.21.7/mdns_sd/ . No GPL source was copied.
macOS app package declares `_peerglide._udp` and the local-network purpose message:
https://developer.apple.com/documentation/bundleresources/information-property-list/nsbonjourservices
and https://developer.apple.com/documentation/bundleresources/information-property-list/nslocalnetworkusagedescription .
These declarations are not permission grants or signing/notarization.

Local complete preflight passes 100 tests. Discovery tests cover metadata/version/
address bounds, floods/exact expiry, GUI address selection preserving authority,
and daemon shutdown with all interfaces disabled on a custom port (no advertisement
or network query). Final Windows/macOS/Linux matrix is a separate gate.
Actual cross-computer multicast, firewall/local-network permission prompts,
network change, sleep and multiple NICs are NOT verified by that lifecycle test.

## Short-code address hints (0.3.6-dev)

The Chinese short-code window has a separate, opt-in browse action for
`_peerglide-pair._tcp.local.`. Old `_peerglide._udp.local.` input-listener hints
cannot be selected as pairing endpoints. Pairing hints require TXT `p=2` plus
input `v=5`, a bounded public fingerprint and the same LAN-address/table limits.
Neither TXT version nor the claimed fingerprint authenticates a peer.

Selecting a result only fills its address and clears the previous code buffer;
it does not send a code, connect, enroll, create an identity or grant anything.
The user enters the code currently displayed by the selected computer and starts
one explicit attempt. There is no automatic trial against multiple servers.
The existing PAKE/mTLS identity binding and both save confirmations remain required.

Showing a code does not advertise it. A separate Chinese button opts into a
public address/version/fingerprint advertisement while the actual owned worker is
Listening. Its fingerprint comes from that worker's frozen local certificate.
No code, OS hostname, username, permission, receipt or private key is published.
Leaving Listening withdraws it; it is not automatically republished after a failed
attempt. Cancel, focus loss, dialog close and Quit request daemon shutdown. The
GUI keeps polling the stop acknowledgement and prevents a replacement daemon
until it completes. Network caches/loss can retain stale hints; a hint never
extends the worker's original deadline or authorizes a connection.

The macOS bundle declares both service types. This is not a local-network
permission grant. Cross-host multicast, platform prompts, adapter changes and
sleep remain native acceptance work. New metadata/expiry and disabled-interface
daemon lifecycle tests, plus address-selection/default-off GUI regressions, do
not replace those checks. Candidate CI evidence is in VALIDATION.md.
