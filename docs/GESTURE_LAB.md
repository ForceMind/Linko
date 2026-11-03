# Offline gesture lab (G04 recording/test slice)

Open **Gesture lab** in the desktop sidebar or launch `peerglide-gui --gesture-lab`.
No identity, account, paired computer, network or OS input permission is required.
The page uses ordinary egui events delivered to its own canvas. It installs no
global hook and executes no keyboard, window, application or remote action.
Stop any device worker before using the lab; a running worker disables the page
and cancels an active candidate. Leaving the page also cancels active recording.

## Try and label a sample

1. Hold the right mouse button inside the canvas, draw and release inside it.
2. Read the matched gesture/distance or explicit rejection. Small clicks, unknown
   shapes and ambiguous shapes can reject; rejection is not an instruction to guess.
3. Choose the **intended** label (including Unknown / negative). This does not alter
   the recognizer or automatically relabel the drawing as its prediction.
4. Choose a sample folder and click **Save labelled sample** to write one JSON file.

Drawing remains in memory until Save; there is no upload or automatic dataset
collection. Each file is created exclusively; one completed drawing can be saved
only once. Starting a new drawing enables another sample. Existing samples are
never overwritten. Save errors are visible; a failed filesystem write can leave
a partial new file, not a silently replaced existing sample. Clear removes only
the in-memory drawing; it does not delete any exported file.

Escape, focus loss, leaving the canvas, page/geometry changes, another button or
wheel, timeout and resource overflow cancel an active candidate. Cancelled or
incomplete prefixes cannot be exported as completed samples. A held candidate
expires even without incoming motion. The library's point/duration/path limits
still apply. The preview is clipped to the canvas and follows the app theme.

## Export provenance and acceptance boundary

Schema 1 includes `source: egui_canvas`, `units: logical_pixels`, `expected_id`
(null for negative), and `samples` of canvas-local `x`, `y`, `elapsed_us`. It has
no screen image, key contents, device identity, application name, account or
participant identifier. Only choose a folder you intend to use for these files.
Only intentionally exported recordings should be shared for later validation.

These are real GUI-event paths when a person draws, but are not raw native mouse
polling-rate recordings. Events in one egui frame can share a timestamp. No result
from this page alone establishes 125/500/1000 Hz, DPI/trackpad/right-click/drag
noninterference, global input arbitration or dangerous-action acceptance.
The planned 5-person 800-positive / 2000-negative corpus still requires a separately
consented recording/label/evaluation protocol; no human dataset was collected by CI.

This independent recording/test slice can precede G03 because it never suppresses
outside-app input. It does not complete G04: application rules, global offline
worker mode, overlays and configuration migration still depend on the shared
broker/native acceptance. G1 is not enabled as a global mouse enhancement.

## Verification

Headless egui frame tests deliver real `PointerButton`/`PointerMoved` input events
to the widget and check recognition, focus cancellation, no-motion expiry and
worker-start cancellation. Additional tests cover geometry/time cancellation,
label-independent recognition and explicit, nonduplicated labelled exports.
Every configuration page renders in both themes, including this page.
Linux CI launches both Home and `--gesture-lab` under isolated Xvfb and stores
PNG screenshots. Windows/macOS window interaction and physical mouse acceptance
remain separate from compile/test/package CI. See VALIDATION.md for actual runs.
