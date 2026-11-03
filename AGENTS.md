# PeerGlide development contract

Read README.md, docs/DEVELOPMENT_PLAN.md, docs/VALIDATION.md and
docs/LOCAL_CODEX_HANDOFF.zh-CN.md before changing behavior. Continue existing
work; do not reset the workspace, renumber milestones or change the MIT license.

- Main is `main`. Inspect current head, open PRs/issues and exact-candidate CI.
- Run fmt, strict workspace Clippy, locked workspace tests and build; run relevant
  process/GUI smokes. Never disable a test/permission gate to make CI pass.
- Physical Windows/macOS/Bluetooth/USB/DPI acceptance is #3/C06. CI and synthetic
  events do not check those boxes. #10 historical root cause remains unconfirmed.
- Native input has one existing capture lifecycle. Never add a competing hook.
  Recognition, allocation, disk/network I/O and scripts must not run in callbacks.
- G03 reference model is not native callback code. Honor passed/suppressed right
  button pair ownership, injected origins, context invalidation and edge rearming.
- G1 global capture/actions remain disabled. Rule preview and mocked backends do
  not authorize execution. Never use fixture tokens/IDs as runtime authentication.
- Keep v5 input/ALPN compatibility unless an ADR and two-peer migration justify a
  change. No remote action/script RPC exists. Input grants imply no data/action grants.
- Keep local recovery/owned-injection cleanup and explicit stopping. Don't log key,
  clipboard or file contents or automatically publish local acceptance reports.
- Do not overwrite invalid/future configs, existing identities or user files.
  GUI config uses bounded versioned store/OS locks; inspect conflicts, don't force.
- User authorization is project development, not deployment, credential purchases,
  automatic software installation, system cleanup or disabling OS security. Explain
  and obtain authorization for additional machine/environment changes as needed.
- Use small branches/PRs, no force pushes, protected-branch bypass or visibility change.
  Record implemented/tested vs native-unverified vs planned work separately.

- GUI minimum Rust is 1.95 (eframe 0.37). Audit advisory warnings are denied;
  do not add blanket ignores. Bundled font licenses/notices accompany distributions.
