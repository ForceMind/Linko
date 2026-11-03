# G02 pure trajectory recognition

`peerglide-gestures` is an independent Rust logic library under ADR-0002. No
native input path uses it yet. The GUI's [offline canvas lab](GESTURE_LAB.md)
uses it without global interception. It has no runtime dependencies, OS hooks,
network, action mappings, mouse suppression or input injection. G02 remains implementing:
the recognizer and synthetic regression suite exist, human acceptance does not.

## Geometry and result

The starter vocabulary has stable IDs: 1 left, 2 right, 3 up, 4 down, 5 down then
right, 6 down then left, 7 up then right, 8 up then left. These are templates, not
enabled bindings. Callers can supply up to 64 templates with arbitrary numeric
IDs. Multiple examples of one ID contribute their minimum score; the runner-up
must have a distinct ID, avoiding false ambiguity within a gesture class.

Each candidate is fully checked before jitter filtering. The filtered path keeps
its endpoint, is sampled at 32 equal arc-length positions, translated by its
bounding-box center and uniformly scaled by its largest extent. No rotation,
reflection, independent axis scaling, path reversal or cyclic starting-point
search is applied. Writing direction and corner order remain significant.
Normalized mean point distance is a geometric score, not a probability. A match
must meet the configured maximum distance and minimum advantage over the second
ID. Ties and near ties reject, regardless of template ordering. A rejection returns
no guessed ID; no result contains permission or an executable action.

| Default / hard bound | Value |
| --- | --- |
| Coordinate convention | Logical pixels, x right / y down, single frozen coordinate space |
| Coordinate magnitude | Finite, absolute value at most 1,000,000 |
| Input / template points | At most 2,048 each; reject oversized input before work |
| Templates / resampled points | At most 64 / fixed 32 |
| Jitter distance | 2 logical pixels |
| Minimum extent / filtered path length | 24 / 40 logical pixels |
| Maximum raw or filtered path length | 20,000 logical pixels |
| Duration | Strictly below 2,500 ms |
| Maximum mean distance / minimum advantage | 0.12 / 0.04 normalized units |

Configuration must be finite and bounded; invalid settings/templates reject at
construction. Work per recognition is bounded by input points plus 64 × 32
comparisons and a bounded ranking step. There is no model download or AI service.
Jitter cannot hide an invalid coordinate, excessive raw movement or late timestamp.
Equal timestamps are allowed; decreasing offsets reject. Sparse paths remain
valid only within all other checks.

## Worker-side collection and cancellation

`Recognizer::stroke()` creates a bounded collector borrowing an immutable
recognizer. Push validates each sample and reserves no more than the fixed
capacity. Invalid input, overflow and cancellation poison the entire stroke,
including any otherwise recognizable prefix. `finish(elapsed)` consumes it and
checks the release time even when no new motion arrived before a timeout.
The slice API expects the caller to provide the whole candidate through release;
use the collector to avoid accidental prefix recognition after an error.

Construct/recognize on a worker, outside a native input callback. The caller must
convert physical/DPI coordinates, freeze/revalidate action context, supply a local
monotonic elapsed clock and cancel on event loss, focus/permission/configuration
change or emergency stop. This library does not know any of those OS events.
At high polling rates the sample bound can be reached before the duration bound;
overflow cancels rather than truncates. This is not 1,000 Hz hardware acceptance.

A normal right-button drag can have exactly the geometry of a gesture. A test
explicitly preserves that fact. Recognition alone cannot determine user intent
or make ordinary right-click/drag noninterference claims. G03 shared arbitration,
exclusions, activation policy and balanced button ownership must precede any native
interception. No second global hook has been introduced. G1 remains disabled.

## Reproducible synthetic evidence

Run `cargo test -p peerglide-gestures --test corpus --locked -- --nocapture`.
CI runs this on Windows/macOS/Linux and prints provenance plus metrics.

`tests/fixtures/trajectories.json` contains 8 hand-authored synthetic positive and
8 synthetic negative paths with labels. A separate fixed-seed generator
(`0x504720261103`) produces 512 translated/scaled/noisy/unevenly sampled positive
paths and 512 selected negatives (small jitter, diagonals, closed circles).
The regression expectation is all 512 correct IDs, zero wrong/rejected positives,
and zero matches among those 512 negatives; fixture expectations are exact.
The generated positives derive from templates and are not an independent human
test set. Metrics must not be represented as measured real-world accuracy.

Unit tests also cover direction/reflection/order, repeated points, sparse/dense
sampling, invalid tails, zero/NaN settings, exact timeout, raw-length limits,
ambiguous IDs, cancellation and collector overflow.

The product plan's 5-person 800 positive / 2,000 negative recordings, >=95%
recognition, <=0.5% false-trigger target, dangerous-action zero misexecution,
physical polling-rate/DPI/trackpad/drag tests and measured resource/latency
budgets are still pending. No human paths were recorded here. C06, G03/G04,
native action execution and real single-machine usability remain separate gates.
