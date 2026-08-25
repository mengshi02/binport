# Changelog

All notable changes to binport are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/mengshi02/binport/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/mengshi02/binport/releases/tag/v0.1.4
[0.1.3]: https://github.com/mengshi02/binport/releases/tag/v0.1.3
[0.1.2]: https://github.com/mengshi02/binport/releases/tag/v0.1.2
[0.1.1]: https://github.com/mengshi02/binport/releases/tag/v0.1.1
[0.1.0]: https://github.com/mengshi02/binport/releases/tag/v0.1.0
