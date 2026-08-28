# Changelog

All notable changes to binport are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-08-28

### Fixed

- Complete password-based guided exec-hop setup by offering to install a
  dedicated key on the entry host instead of printing a command that cannot
  authenticate after the temporary password is discarded.
- Report the actual exec-hop entry host in authentication recovery hints.
- Preserve `--user` and `--port` overrides during interactive host setup.
- Replace the shared-credential SSH fixture with isolated entry and target
  servers, and exercise guided setup, execution, file transfer, removal, TCP
  relay, and entry-authentication failures end to end.

## [0.2.0] - 2026-08-28

### Added

- Add `binport host add NAME` guided setup for fresh clients, covering direct
  SSH, reusable jump hosts, composite-username enterprise bastions, and a
  bounded Auto detect flow while preserving the existing scripted form.
- Add a shared host capability probe that verifies marked command execution,
  lossless stdin/stdout file streaming, and `direct-tcpip`, with per-capability
  timing and failure details in text and JSON reports.
- Probe guided jump routes before saving, separating entry-host authentication,
  forwarding policy, and target authentication so remote-only target credentials
  are reported as an exec-hop candidate instead of a generic key failure.
- Add the versioned native Rust `binport-hop` protocol and helper, including
  bounded request framing, binary stdin, target exit/stdout/stderr propagation,
  release-checksum verification, content-addressed deployment, and non-TTY
  single-host toolbox execution using credentials held on the entry host.
- Route `cp` uploads/downloads and `rm` through exec-hop, with exact download
  size validation and bounded 64 KiB incremental payload streaming across both
  SSH legs; extend Linux SSH CI with a target key available only to the entry
  host.
- Add single-connection TCP relay over exec-hop, using one native helper channel
  for each accepted local connection with bounded backpressure and half-close
  propagation; cover it with an HTTP tunnel in the dual-SSH Linux E2E test.
- Add deployment-verified bastion compatibility presets, including
  `h3c-iware-slash`, via `binport bastion presets` and
  `binport host add --bastion-preset`.
- Add vendor-documented Huawei Cloud CBH and community-reported JumpServer/Koko
  presets with explicit trust status and source metadata.
- Add foreign PAM presets for One Identity SPS, WALLIX Bastion, and CyberArk
  PSMP, retaining vendor-documented versus community-reported provenance.
- Add `binport bastion probe` for bounded connection and exec checks, with an
  opt-in `--check-forwarding` direct-tcpip capability test.
- Add a credential-free localhost SSH end-to-end test covering execution,
  content-addressed cache hits, fleet commands, file copies, and removal in CI.

### Changed

- Reposition binport as an agentless remote toolbox for direct SSH hosts,
  ProxyJump routes, enterprise bastions, and forwarding-restricted exec-hop
  environments.
- Allow native SSH Agent authentication for application-layer bastion routes.
- Restructure both READMEs around guided routing and exec-hop, add a complete
  Chinese v0.2 user guide, and remove recording-only sources from user-facing
  documentation.
- Move the `host`, `auth`, local toolbox lifecycle, remote file transfer, and
  OCI Registry command implementations out of the binary entry point into
  focused modules without changing their CLI interfaces.
- Extract shared route and toolbox artifact resolution from the command entry
  point and move the zero-connection `plan` command into its own module.
- Move `doctor`, `warm`, and shared ProxyJump connection preparation into a
  dedicated fleet module while preserving command output and cache behavior.
- Move the `watch` state machine, reconnect handling, and event rendering into
  a dedicated command module backed by the shared fleet connection pool.
- Move single-host, TTY, streaming, and fleet remote execution into a focused
  command module, leaving the binary entry point responsible only for CLI
  definition, routing, and process exit handling.

## [0.1.5] - 2026-08-26

### Added

- Add `micro` and the `edit` alias for interactive remote file editing.
- Add native-SSH `binport cp` for local-to-remote, remote-to-local, and
  remote-to-remote regular-file copies.
- Show traditional command mappings and descriptions in `binport ls`.
- Add reusable terminal transfer progress with byte counts, throughput, and ETA
  for file copies, downloads, and first-run tool uploads.
- Add guarded remote deletion with `binport rm HOST:PATH`, explicit recursive
  directory removal, force mode, JSON output, and dangerous-path rejection.
- Add `binport host add|ls|show|test|remove` backed by a standard managed SSH
  config fragment, including one-hop ProxyJump routes and conflict protection.
- Support managed-key authentication setup, status, and removal through a
  key-authenticated one-hop ProxyJump.

### Changed

- Give remote `eza` human-friendly long-format and color defaults without
  overriding explicit user options.
- Move the curated tool metadata from Rust source into a strictly validated,
  compile-time embedded `catalog.yaml` while preserving offline single-binary
  operation.
- Stream `binport cp` in bounded 64 KiB chunks instead of buffering complete
  files in memory.
- Move file-operation shell command construction out of `main.rs` into a tested
  remote command module.

## [0.1.4] - 2026-08-25

### Added

- Add interactive SSH PTY mode with raw keyboard forwarding via `--tty`.
- Automatically allocate a PTY for the `btm` system monitor.
- Add ad-hoc one-hop routes with `binport JUMP,TARGET TOOL ...`.

## [0.1.3] - 2026-08-25

### Added

- Expand the modern Unix catalog with `bat`, `dust`, `btm` (bottom), `sd`,
  and `delta`.

## [0.1.2] - 2026-08-25

### Added

- Add `eza` 0.23.5 to the curated catalog for modern remote directory listings.

## [0.1.1] - 2026-08-25

### Added

- Manage dedicated, per-host Ed25519 keys with `binport auth setup`, `status`,
  and `remove` without persisting SSH passwords.
- Run binport as a native Windows amd64 client, with a checksum-verifying
  PowerShell installer and Windows release archive.

### Changed

- Discover binport-managed keys automatically and include an actionable auth
  setup hint when key authentication fails.
- Use native Windows configuration and cache directories when `HOME` is not
  available.

## [0.1.0] - 2026-08-25

### Added

- Build reproducible, multi-platform toolboxes from a `Binfile` and
  `Binport.lock`.
- Run toolbox binaries over native Rust SSH without installing binport on the
  destination.
- SSH config aliases, one-hop ProxyJump, fleet execution, planning, health
  checks, cache warming, and change-aware watch mode.
- Offline toolbox export/import and OCI image layout pack/unpack.
- Anonymous and authenticated OCI Registry pull/push for Harbor-compatible
  registries.
- JSON output for automation and JSON Lines output for watch events.

### Fixed

- Prefer an explicit SSH `IdentityFile` over an available but empty SSH agent.

[Unreleased]: https://github.com/mengshi02/binport/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/mengshi02/binport/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/mengshi02/binport/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/mengshi02/binport/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/mengshi02/binport/releases/tag/v0.1.4
[0.1.3]: https://github.com/mengshi02/binport/releases/tag/v0.1.3
[0.1.2]: https://github.com/mengshi02/binport/releases/tag/v0.1.2
[0.1.1]: https://github.com/mengshi02/binport/releases/tag/v0.1.1
[0.1.0]: https://github.com/mengshi02/binport/releases/tag/v0.1.0
