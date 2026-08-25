# Changelog

All notable changes to binport are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/mengshi02/binport/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mengshi02/binport/releases/tag/v0.1.0
