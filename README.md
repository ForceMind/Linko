# PeerGlide · 屏跃

**Multiple Computers. One Seamless Experience.**

Rust software KVM for independent computers. Each peer can capture local input
and receive authorized remote input. Bluetooth and USB devices stay paired
with their original computer; operating system input is forwarded over LAN.

## Status

Experimental **0.1.0-dev**, not a stable release. CLI, authenticated QUIC transport,
bounded protocol and recovery state machine are runnable. Windows/macOS native
adapters are implemented; real Windows ↔ macOS dual-machine acceptance is
**pending**. Linux currently supports transport/protocol diagnostics only.
See [validation](docs/VALIDATION.md).

Primary-display switching, basic HID keys, three mouse buttons and scrolling
form the initial native prototype. Multi-monitor topology, IME composition,
clipboard, files, mDNS, pairing codes, tray and signed installers are
not implemented in this milestone. A native eframe control panel provides
identity/enrollment, session start/stop, probes, relative screen placement and
dark/light settings.

## Build

```sh
cargo build --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Use stable Rust. Run `cargo build --workspace --release --locked` to build both
executables. Launch `peerglide-gui` with `peerglide` beside it. Devices creates
an identity and enrolls the other computer's public certificate after fingerprint
confirmation. Settings explicitly enables native input and receiving control;
Home starts/listens/connects and can run a harmless transport probe. Displays
sets the switching side. No config file editing is required.

CI artifacts contain an unsigned Windows portable ZIP, macOS app bundle or
Linux portable ZIP plus checksums and notices. Linux GUI runtime libraries
include X11/XKB, EGL/OpenGL (or the corresponding Wayland stack). They are experimental builds,
not stable installers. macOS signing/notarization and Windows code signing are
pending; do not disable OS protections to open them. The Linux GUI can configure
and probe connections, but Linux native input is not implemented.

## Explicit two-peer enrollment

On each computer create a **different** identity:

```sh
peerglide init --identity identity
```

Exchange only `node.crt.der` through a separately trusted channel. Independently
compare its printed BLAKE3 fingerprint with the other computer. Never share
`node.key.der`. Store the other certificate as `peer.crt.der`. This manual
enrollment precedes the future pairing GUI. There is no blind TOFU mode.

Test without input permission, on A:

```sh
peerglide listen --peer-cert peer.crt.der
```

On B (replace the LAN IP):

```sh
peerglide connect 192.168.1.10:24870 --peer-cert peer.crt.der --bind 0.0.0.0:0 --probe
```

`--probe` checks mutual TLS, version negotiation and a reliable roundtrip without
injecting input. Permit UDP 24870 for the process if the OS firewall prompts;
do not disable the firewall. By default the CLI exits on disconnect; optional
trusted reconnect is described below.

## Experimental native control

Both ends use `--native`. Add `--allow-control` on each computer permitted to
receive remote input. Listener/connector roles only establish the connection;
they do not create a permanent input master. `--edge` specifies the peer's side.

For A left of B:

```sh
# A
peerglide listen --peer-cert peer.crt.der --native --allow-control --edge right
# B
peerglide connect 192.168.1.10:24870 --peer-cert peer.crt.der --bind 0.0.0.0:0 --native --allow-control --edge left
```

- Release held keys/buttons before crossing the primary display edge.
- Keyboard follows the pointer. Move back over the remote entry edge to return. Edge rearming requires interior movement.
- **Ctrl+Alt+Right**, then release the chord, toggles the local control target.
- **Ctrl+Alt+Esc** immediately stops capture and requests remote release.
- Physical input at the receiver reclaims control.
- Capture watchdog: 1 second. Receiver lease: 1.5 seconds. Exit/error paths
  release tracked remote keys and buttons.

macOS requires Accessibility and Input Monitoring permission for the exact
app/terminal running the prototype, followed by restart. Secure Input remains
outside its control. Windows input cannot cross privilege boundaries or control
UAC/secure desktop. Keep OS security protections enabled.

## Documentation

- [Product](docs/PRODUCT.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Roadmap](docs/ROADMAP.md) and [development plan](docs/DEVELOPMENT_PLAN.md)
- [Research and licensing](docs/REFERENCE_RESEARCH.md)
- [Security](docs/SECURITY.md) and [validation](docs/VALIDATION.md)

MIT licensed. GPL references are studied, not copied or linked. Dependency
licenses are separate and must accompany distributions.


Protocol migration: current development builds use `peerglide/5`. Update both
computers together; v3/v4 peers are rejected before input capture. Existing identity
and enrolled certificates remain usable. See [ADR-0003](docs/ADR/0003-terminal-cursor-v4.md).

Input capability negotiation and v5 migration are documented in [ADR-0004](docs/ADR/0004-input-capabilities-v5.md).


Trusted reconnect is optional and off by default. Enable **Reconnect trusted peer
automatically** in Settings, or add `--reconnect` to `listen`/`connect`. It retries
the same enrolled identity with capped backoff; changing certificates, native
cleanup errors or authorization failures stop it. Control stays local until you
switch again. See [ADR-0005](docs/ADR/0005-trusted-reconnect.md) for limits.
