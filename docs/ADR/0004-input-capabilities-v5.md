# ADR-0004: input capabilities before capture (protocol v5)

Date: 2026-11-02. Status: implemented; native CI and physical acceptance recorded separately.

A trusted connection is not evidence that the receiving backend can inject every
input. Hello now carries a fixed 256-bit HID keyboard bitmap, three-button mask,
motion and wheel support. Reserved bits and malformed shapes are rejected before
capture; capabilities are immutable per session and never imply authorization.
Only native-enabled endpoints advertise their compiled backend support. Diagnostic
and Linux endpoints advertise no input; even --allow-control cannot override that.
OS permission availability is still checked separately when starting native input.

The source callback checks target keys/buttons/wheel before returning its suppression
decision. Unsupported input disables suppression and is delivered locally, with a
payload-free notification and release of the old remote lease. Held-state tracking
still guards the next acquisition. Emergency input has priority; unavailable motion
does not reserve the switching arrow. Session sending checks capabilities again,
and the receiver rejects active-lease input outside its advertised set before
injection. The ordinary trust/allow-control gate remains independent.

The required Hello format changes VERSION/ALPN to 5/peerglide/5. Both peers must
update together; v3/v4 endpoints are rejected before capture, with no silent
downgrade. Identity, enrolled certificates and grants are retained. Protocol v4
reliable terminal cursor and cleanup ordering are preserved. GUI-worker IPC stays
v1. Future optional metadata needs an explicit compatibility review; this does
not promise arbitrary unknown-message compatibility or remote action execution.

This is the input slice of P01, not clipboard/file/action capability negotiation,
interactive pairing, discovery or reconnect. Full OEM/media/IME support and native
Win/Mac acceptance remain pending. Bitmaps describe support, not permission proof.
