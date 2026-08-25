# Security

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private security advisory form for this repository instead.

Include the affected version, reproduction steps, impact, and any suggested
mitigation. Reports will be acknowledged as soon as practical.

## Security model

binport verifies downloaded and imported artifacts by SHA-256, validates OCI
descriptor sizes and digests, checks SSH host keys against `known_hosts`, and
does not persist passwords entered at interactive prompts.

Registry credentials and SSH passwords should be scoped to the minimum required
permissions. Prefer a read-only Harbor Robot Account for pull-only systems and
a separate publisher account for push operations.

Keys created by `binport auth setup` are unencrypted, dedicated to one SSH
destination, and stored in the user's binport configuration directory. On Unix,
private keys and key directories are created with mode 0600 and 0700
respectively. Passwords used during setup are never persisted.
