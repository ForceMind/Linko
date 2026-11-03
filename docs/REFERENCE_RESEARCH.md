# Reference research

Inspected 2026-10-22. This is a scoped source/design review, not a complete audit of every dependency. No source from these implementations is copied or linked into PeerGlide.

| Reference | Snapshot | License evidence | Decision |
|---|---|---|---|
| [Lan Mouse](https://github.com/feschber/lan-mouse) | `f1b96bdcc0294d285bc9050b7e146800a3ef8796` | LICENSE GPLv3; Cargo.toml GPL-3.0-or-later | Study only; do not link capture/emulation crates |
| [Deskflow](https://github.com/deskflow/deskflow) | `c1b19244a37bd238038ea2bf0ab56d9efc2de5d7` | LICENSE GPLv2; LICENSES contains GPL-2.0-only, LGPL-2.1-only, OpenSSL exception and MIT texts | Study only; per-file licensing differs |
| [Input Leap](https://github.com/input-leap/input-leap) | `34a34fb20b93113a6b26052cb5a54f9be2327775` | LICENSE GPLv2 with OpenSSL exception; protocol header refers to GPL | Study only; repository is archived at inspection |
| [ShareMouse](https://www.sharemouse.com/) | Public website at inspection | Commercial; no implementation source reviewed | UX ideas only; no nonpublic implementation assumptions |

## Lan Mouse

Read Cargo.toml, LICENSE, input-capture/src/windows/event_thread.rs,
input-capture/src/macos.rs, lan-mouse-proto/src/lib.rs, src/capture.rs and
src/emulation.rs. Workspace separates input capture, emulation, event vocabulary,
IPC and frontends. Windows uses message-loop low-level hooks; macOS uses an event
tap and display bounds. Its protocol has enter/leave acknowledgement and liveness
events. Capture release paths include explicit release, send failure, remote
leave and destruction. Certificate fingerprints are surfaced for authorization.
The manifest uses Tokio and DTLS-related dependencies; it is not Quinn code.

Borrow the general ideas of adapter boundaries, event origin handling, explicit
transition acknowledgements and timeout release. PeerGlide uses an independently
authored JSON protocol with bounded frames, normalized absolute QUIC datagrams,
mutual TLS enrollment and receiver-authoritative short leases.

## Deskflow

Read LICENSE, docs/dev/protocol_reference.md and src/lib/server/Server.cpp.
The protocol documentation distinguishes network client/server from input
primary/secondary; it would be inaccurate to claim Deskflow can never separate
those roles. It describes TCP, TLS, connected/active states, display information,
clipboard and keepalive messages. The server has switch delays, two-tap arming,
corner handling, input event handlers and disconnect cleanup.

Borrow topology as explicit data, edge hysteresis and safe release requirements.
Do not reuse wire identifiers, C++ logic or GUI assets. PeerGlide's stronger
peer-oriented goal is symmetric input grants without permanently assigning an
input primary, not a claim that network-role separation is novel.

## Input Leap

Read LICENSE, src/lib/inputleap/protocol_types.h and src/lib/server/Server.cpp.
The header describes enter/leave, key press/release, clipboard sequence context,
resolution acknowledgement and keepalive expectations. The server implements
screen switching, delay/two-tap gates and client-disconnect handling. Treat it as
mature historical evidence, not an actively maintained dependency recommendation.

## ShareMouse

The public site describes bidirectional Windows/macOS control, edge navigation,
clipboard sharing and cross-computer file drag-and-drop. These are UX targets,
not evidence that PeerGlide already provides them. Pairing, monitor arrangement
and clear trust status should be easy to understand. Native drag must receive its
own implementation and acceptance tests; ordinary file sends cannot substitute.

## Dependency policy

PeerGlide source is MIT. Direct Rust dependencies are independently sourced from
Cargo; record their exact resolved versions and SPDX metadata from Cargo.lock /
cargo metadata. Rustls/Quinn/Tokio/Serde/rcgen, BLAKE3, clap and windows-sys do not
inherit licenses from these reference apps. Transitive dependencies, bundled
native code and their notices require separate distribution review. Dependency
metadata is a screening aid, not a substitute for inspecting shipped notices.


## mDNS implementation dependency

`mdns-sd` 0.21.7 (https://github.com/keepsimple1/mdns-sd), licensed Apache-2.0 OR MIT
per locked published Cargo metadata, is linked with default features disabled.
API behavior reviewed at https://docs.rs/mdns-sd/0.21.7/mdns_sd/ and local published
source; no reference GPL implementation was copied. Discovery hints never establish
trust; collector/notice screening includes the dependency. See DISCOVERY.md for
actual bounded hint policy and unresolved daemon-cache/network acceptance limits.
