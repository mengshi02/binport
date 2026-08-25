# Contributing to binport

Thanks for helping make portable command-line tools easier to use across SSH
fleets. Bug reports, focused features, documentation, tests, and additions to
the curated tool catalog are welcome.

## Before opening a change

- Search existing issues and pull requests to avoid duplicate work.
- Open an issue before a large feature or architecture change.
- Report security problems privately as described in [SECURITY.md](SECURITY.md).
- Keep each pull request focused on one problem.

## Development setup

Install the current stable Rust toolchain with `rustfmt` and Clippy, then run:

```sh
cargo build --locked
cargo test --all-targets --all-features --locked
```

Before submitting a pull request, run the same checks as CI:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked
```

Tests that require a real SSH host or private OCI Registry must remain opt-in.
Never commit credentials, private hostnames, production IP addresses, SSH
configuration, downloaded toolbox binaries, or local `.binport` state.

## Making a change

- Add or update tests for observable behavior.
- Preserve unmodified remote stdout, stderr, and exit codes.
- Treat tool names, paths, SSH aliases, Registry responses, and remote output as
  untrusted input.
- Avoid invoking external `ssh`, `scp`, or shell tools for core transport.
- Update the README and changelog when behavior visible to users changes.
- Keep `Binport.lock` in sync when changing the repository `Binfile`.

For catalog additions, use official release URLs, immutable versions, SHA-256
checksums, and statically linked Linux amd64/arm64 binaries where available.

## Pull requests

Explain the problem, the chosen approach, verification performed, and any
compatibility or security impact. By contributing, you agree that your changes
are licensed under the repository's MIT license.
