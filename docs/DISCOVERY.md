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
which clears the old peer's grant. No short-code authentication or automatic trust
is implemented. A hint does not mean an authenticated/connected computer.

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
PeerGlide GUI still requires at least Rust 1.89. Existing notice collection includes
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
