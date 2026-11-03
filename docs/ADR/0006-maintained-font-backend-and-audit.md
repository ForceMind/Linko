# ADR-0006: maintained font backend and strict dependency advisory checks

Status: accepted for the development branch; native matrix is a merge gate.

## Context

RustSec RUSTSEC-2026-0192 marks ttf-parser unmaintained (informational, not an
exploitable vulnerability finding), recommends skrifa and lists no patched ttf-parser
release. The eframe 0.33 font stack retains it. A lockfile audit is a point-in-time
check, not an independent security audit. Do not suppress advisory warnings.

Source: https://rustsec.org/advisories/RUSTSEC-2026-0192.html . Actual resolved
Cargo metadata and published upstream sources determine package versions/licenses.

## Decision

Upgrade the existing eframe/egui framework to 0.37.0, using its maintained skrifa
font backend. Keep glow, X11, Wayland, bundled fonts and clipboard integration.
Disable default features, including automatic system-font loading. GUI minimum Rust
is now 1.95; workspace core minimum is unchanged. Update both portable executables
from the same tested build. Input wire version/ALPN v5 and enrollment stay unchanged.

Migrate eframe App to logic/ui: worker and discovery polling run in logic even when
UI is hidden, with bounded repaint requests; focus loss/minimize cancels lab drawing.
Headless tests retain shape/page/pointer assertions and explicitly discard unused
texture deltas through the framework API (no GPU is present in these tests).
Real rendering/export remains covered separately by Linux Xvfb smoke.

CI runs pinned cargo-audit 0.22.2, locked installation, no default binary-scanning
features, denying all advisory warnings and vulnerabilities. It uploads the JSON
result even on failure. There are no advisory ignores or platform filters.
clipboard-win 5.4.3 already replaced the yanked 5.4.1 lock entry in #29.

## Notices and limits

Bundled fonts include OFL.txt, UFL.txt, Hack-Regular.txt and egui-icons.txt. The
old name-only collector omitted these when a differently named license was found.
Collect .txt notices under fonts as well as license/copyright/notice filenames.
A fixture reproduces the omission and verifies all notices; SPDX AND/OR regression
keeps every required term. This remains automated screening, not legal advice or a
complete distribution audit. Neither framework upgrade nor clean audit verifies
Windows/macOS desktop permissions, real hardware, human gesture accuracy or #10's
historical filesystem root cause. Signed installation and native C06 stay pending.
