# binport

[English](README.md) | [简体中文](README.zh-CN.md)

**Build a toolbox once. Run it on any SSH host. Install nothing there.**

![binport terminal demo](docs/demo.svg)

```console
$ binport build .
$ binport prod rg "authentication timeout" /var/log
```

`binport` builds a versioned toolbox of portable command-line programs, selects
the binary matching the remote Linux host, and transfers only that tool over a
native Rust SSH connection. The remote host needs no `binport`, `rg`, daemon,
container runtime, package manager, or root access.

## Install

Download the latest checksum-verified release for Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/mengshi02/binport/main/install.sh | sh
```

The installer detects amd64/arm64, verifies the release against `SHA256SUMS`,
and never invokes sudo. Override its defaults when needed:

```sh
BINPORT_INSTALL_DIR="$HOME/bin" \
BINPORT_VERSION="v0.1.3" \
sh install.sh
```

On Windows amd64, run in PowerShell:

```powershell
irm https://raw.githubusercontent.com/mengshi02/binport/main/install.ps1 | iex
```

The Windows installer places `binport.exe` in
`%LOCALAPPDATA%\binport\bin` by default and prints a reminder if that directory
is not already in the user PATH.

Air-gapped mirrors can set `BINPORT_RELEASE_URL` to a directory containing the
platform archives and `SHA256SUMS`.

Build from source with a recent stable Rust toolchain:

```sh
cargo install --path . --locked
```

## Passwordless authentication

If a host currently requires a password, let binport create and install a
dedicated Ed25519 key:

```console
$ binport auth setup server-a
SSH password:
Passwordless authentication is ready for server-a

$ binport server-a rg --version
```

The password is used for that connection only and is never persisted. The
private key is stored with restrictive permissions under the platform's
binport configuration directory, separate from existing SSH keys. Installation
is idempotent.

```sh
binport auth status server-a
binport auth remove server-a
```

`remove` deletes the exact managed public-key line remotely before deleting the
local key. Auth management currently supports direct hosts; existing key and
password authentication continue to work through one-hop ProxyJump.

## Binfile

```dockerfile
TARGET linux/amd64
TARGET linux/arm64

TOOL rg@15.2.0
TOOL fd@10.4.2
TOOL jq@1.8.2

# Bring your own statically linked tool.
COPY ./target/x86_64-unknown-linux-musl/release/logscan logscan --target linux/amd64
```

Resolve immutable sources, then build all declared platforms:

```console
$ binport resolve .
Resolved 6 artifacts into Binport.lock

$ binport build .
binport: fetching rg@15.2.0 for linux/amd64
binport: fetching fd@10.4.2 for linux/amd64
...

Toolbox built: 6 artifacts
Manifest: .binport/toolbox.json
```

Downloads come from the tools' official releases and are verified against
pinned SHA-256 checksums. Downloads are cached locally; remote binaries use a
content-addressed cache.

`Binport.lock` records the exact source URL, source checksum, archive format,
version, and platform for every artifact. Commit it with the project. `build`
consumes the lock and rejects a changed Binfile or changed local `COPY` input
until `binport resolve` is run again. For compatibility, the first build creates
a missing lock automatically.

## Commands

```text
binport resolve [PATH]            Resolve Binfile into Binport.lock
binport auth setup HOST           Install a dedicated passwordless SSH key
binport auth status HOST          Verify the managed key
binport auth remove HOST          Remove the key locally and remotely
binport build [PATH]              Build the Binfile
binport ls [PATH]                 List declared tools (`list` is an alias)
binport fetch TOOL...             Pre-download tools
binport fetch --all               Pre-download the curated catalog
binport status [PATH]             Show toolbox and cache status
binport clean                     Remove the download cache
binport export ops.toolbox        Export one self-contained offline file
binport load ops.toolbox          Load an offline toolbox
binport pack ops.oci              Pack a local OCI image layout
binport unpack ops.oci            Restore a local OCI image layout
binport pull oci://HOST/REPO:TAG  Pull and install an OCI toolbox
binport push oci://HOST/REPO:TAG  Publish the built toolbox
binport doctor HOST|@GROUP        Check routes, platforms, latency, and cache
binport warm HOST|@GROUP          Preload every missing toolbox artifact
binport plan HOST|@GROUP TOOL     Preview hosts, routes, and artifacts offline
binport watch [OPTIONS] HOST TOOL Repeatedly report command-result changes
binport HOST TOOL [ARGUMENTS]...  Execute a tool remotely
binport @GROUP TOOL [ARGUMENTS]... Execute concurrently across a fleet
```

## OCI toolbox artifacts

A built toolbox can be represented as a standard local OCI image layout before
it is pushed to GHCR or Harbor:

```sh
binport resolve .
binport build .
binport pack ops.oci

# On a clean machine or jump host:
binport unpack ops.oci
binport ls
binport warm @prod
```

The layout contains an OCI index, one manifest per Linux platform, and one
content-addressed layer per tool:

```text
ops.oci/
├── oci-layout
├── index.json
└── blobs/sha256/
```

Separate tool layers allow a Registry to deduplicate unchanged binaries across
toolbox versions. Every descriptor size and SHA-256 digest is verified while
unpacking. The exact `Binport.lock` is embedded in each platform config and is
restored with the toolbox, so imported artifacts retain their provenance.

`unpack` consumers do not need the original Binfile: `ls`, `status`, remote
execution, `doctor`, and `warm` operate from the imported toolbox manifest.

Pull the same layout from an OCI Distribution Registry:

```sh
binport pull oci://ghcr.io/acme/toolboxes/ops:v1
binport pull oci://harbor.internal/binport/ops@sha256:<digest>
```

`pull` resolves a tag once, then fetches platform manifests and blobs by
digest. It currently materializes `linux/amd64` and `linux/arm64`, verifies
every descriptor size and SHA-256, caches blobs globally, and installs the
toolbox atomically. Public Registries using an anonymous Bearer-token challenge
are supported. It never invokes Docker, ORAS, or curl.

Private Harbor-compatible Registries use an in-memory credential prompt:

```sh
binport push oci://harbor.internal/acme/ops:v1 \
  --username 'robot$binport' --registry-password

binport pull oci://harbor.internal/acme/ops:v1 \
  --username 'robot$binport' --registry-password
```

Credentials are used for Basic-to-Bearer token exchange and are not written to
the project or binport configuration. Push checks every blob with `HEAD`,
uploads only missing content, publishes platform manifests by digest, and then
publishes the multi-platform index under the requested tag. Repeating a push of
the same digest is treated as success, including on Registries with immutable
tags.

Unencrypted development Registries require an explicit opt-in:

```sh
binport pull --plain-http oci://127.0.0.1:5000/acme/ops:v1
```

HTTPS remains the default. Persistent credential helpers and custom certificate
authorities are the next Registry phase.

## Offline and custom tools

`COPY` adds an existing executable to one target without requiring a catalog
entry. Paths are resolved relative to the `Binfile`:

```dockerfile
COPY ./dist/linux-amd64/logscan logscan --target linux/amd64
COPY ./dist/linux-arm64/logscan logscan --target linux/arm64
```

Export the complete toolbox, move the single file to a disconnected jump host,
and load it there:

```sh
binport build .
binport export ops.toolbox

# On the offline jump host:
binport load ops.toolbox
binport server-a rg timeout /var/log
```

The imported manifest and every executable are checked against the SHA-256
recorded at build time. Archive paths are constrained to the destination
project during extraction.

Examples:

```sh
binport prod jq . /srv/app/config.json
binport deploy@example.com fd '\.log$' /var/log
binport server-a rg 'panic|fatal' /srv
binport --password root@server-a rg timeout /var/log
```

`HOST` can be `user@hostname` or an exact alias in `~/.ssh/config`:

```sshconfig
Host server-a
    HostName 192.0.2.15
    User deploy
    Port 22
    IdentityFile ~/.ssh/id_ed25519
```

Hosts behind a jump server use the standard `ProxyJump` field:

```sshconfig
Host bastion
    HostName 203.0.113.10
    User deploy

Host server-a
    HostName 192.0.2.15
    User deploy
    ProxyJump bastion
```

`binport server-a rg ...` then creates the target SSH connection through a
native `direct-tcpip` channel on `bastion`; neither connection invokes an
external SSH process.

## Fleet execution

Concrete SSH aliases double as a zero-setup host inventory:

```sshconfig
Host bastion
    HostName 203.0.113.10
    User deploy

Host prod-api-01
    HostName 192.0.2.15
    User deploy
    ProxyJump bastion

Host prod-api-02
    HostName 192.0.2.16
    User deploy
    ProxyJump bastion
```

Run one toolbox command across every `prod-*` host:

```console
$ binport --password --concurrency 20 @prod rg 'panic|fatal' /var/log
prod-api-01  /var/log/app.log: fatal: connection refused
prod-api-02  /var/log/worker.log: panic: task timed out

2 hosts · 2 succeeded · 0 failed · 1 jump reused · 0.42s
```

`@prod` selects concrete aliases named `prod` or beginning with `prod-`;
`@all` selects every concrete alias. Wildcard `Host` entries still provide SSH
defaults but are not enumerable inventory members. Each host is isolated, so a
failed connection does not stop the others. `--password` prompts once and uses
the entered password for the selected hosts and their jump server.
All targets using the same `ProxyJump` share one authenticated jump connection;
each target gets its own native `direct-tcpip` channel over that connection.

Fleet output is streamed as commands run. SSH chunks are reassembled into
complete lines and prefixed with the host alias, so long-running commands show
progress without mixing partial lines from different machines.

Agents and automation can request one deterministic JSON document:

```console
$ binport --json @prod rg 'panic|fatal' /var/log
{
  "results": [
    {
      "host": "prod-api-01",
      "platform": "linux/amd64",
      "cache_hit": true,
      "status": 0,
      "stdout": "...",
      "stderr": "...",
      "ok": true
    }
  ],
  "summary": {
    "hosts": 2,
    "succeeded": 2,
    "failed": 0,
    "jumps_reused": 1,
    "elapsed_seconds": 0.42
  }
}
```

`--json` also works with a single host and `doctor`. Human output streams by
default; JSON mode buffers results until completion so stdout always remains
valid JSON.

Preflight a fleet before an incident or deployment:

```console
$ binport --password doctor @prod
HOST            ROUTE          PLATFORM      LATENCY  CACHE
prod-api-01     bastion        linux/amd64      199ms  2/3
prod-api-02     bastion        linux/amd64      219ms  2/3
prod-worker-01  direct         linux/amd64      229ms  2/3
```

`CACHE` reports how many artifacts for that platform already exist in the
remote content-addressed cache. `doctor` performs checks concurrently and
shares jump connections just like Fleet execution.

Preload missing tools before an incident or deployment:

```console
$ binport --password warm @prod
HOST            ROUTE          PLATFORM      CACHED  UPLOADED  TRANSFER
prod-api-01     bastion        linux/amd64        2         1   2.2MiB
prod-worker-01  direct         linux/amd64        3         0        0B
```

`warm` is incremental and idempotent: it checks every content hash in one
remote request, uploads only missing artifacts, verifies per-host failures, and
supports `--json`. A second run transfers zero bytes.

Preview an operation without opening any network connection:

```console
$ binport plan @prod rg
HOST            DESTINATION              ROUTE
prod-api-01     deploy@192.0.2.15:22      bastion
prod-api-02     deploy@192.0.2.16:22      bastion

ARTIFACT        SIZE      REMOTE CACHE PATH
linux/amd64     5.2MiB    $HOME/.cache/binport/<sha256>/rg
linux/arm64     4.3MiB    $HOME/.cache/binport/<sha256>/rg

Plan only · no network connections made
```

This makes host selection, jump routing, artifact choice, and transfer identity
auditable before execution. `binport --json plan ...` provides the same plan to
CI policies and agents.

Continuously watch a fleet while keeping its SSH sessions open:

```console
$ binport watch --interval 5 @prod rg 'panic|fatal' /var/log/app.log
Watching 24 hosts every 5s · Ctrl-C to stop

+    0.2s  api-03  INITIAL
api-03  fatal: database timeout
+   10.0s  api-07  OFFLINE
api-07  connection timed out
+   20.0s  api-07  RECOVERED
+   25.0s  api-03  CLEARED
```

Watch reports `INITIAL`, `CHANGED`, `CLEARED`, `OFFLINE`, and `RECOVERED`
events and suppresses unchanged snapshots by default. Connections—including a
shared ProxyJump connection—are reused between snapshots. Failed target
connections are retried on the next interval.

Useful control modes:

```sh
binport watch --count 20 --interval 2 @prod rg timeout /var/log/app.log
binport watch --until-success @prod rg 'deployment complete' /var/log/deploy.log
binport watch --all --count 3 server-a jq .status /srv/health.json
binport watch --jsonl @prod rg panic /var/log/app.log
```

`--jsonl` emits one self-contained event per line for agents and streaming
systems. Ctrl-C exits cleanly; a finite watch returns the status of its final
snapshot.

## How remote execution works

For every invocation, binport:

1. Resolves the destination and credentials from the SSH agent and the common
   `HostName`, `User`, `Port`, and `IdentityFile` SSH config fields.
2. Opens one native Rust SSH connection—no external `ssh` process.
3. Runs one bootstrap request that detects OS/CPU, selects the matching
   artifact, checks the content-addressed cache, and immediately executes a
   cached tool.
4. If missing, streams the tool over another channel, atomically installs it,
   and executes it.
5. Returns the tool's unmodified output and exit status.

The cache-hit path takes one remote request after the SSH connection is ready.
The bootstrap uses POSIX shell primitives and does not require an agent,
runtime, or helper program on the host.

The remote cache lives under `~/.cache/binport/<sha256>/` and subsequent runs do
not transfer the executable again.

## Current scope

This is an early Linux-remote-focused release:

- Curated tools: `rg`, `fd`, `jq`, `eza`, `bat`, `dust`, `btm` (bottom),
  `sd`, and `delta`. The Linux amd64 artifacts are static; the upstream arm64
  artifacts for `eza` and `delta` require glibc.
- Targets: Linux amd64 and Linux arm64.
- Clients: Linux and macOS on amd64/arm64, plus Windows amd64.
- Authentication: SSH agent, unencrypted private key, or an interactive
  password prompt. Dedicated per-host keys can be managed with `binport auth`.
- SSH config: exact host aliases plus `HostName`, `User`, `Port`, and
  `IdentityFile` (simple `Host *` defaults are supported).
- Fleet groups: concrete SSH aliases selected by prefix, with bounded parallel
  execution (`--concurrency`, default 10) and a per-host result summary.
- One-hop `ProxyJump` is supported. Comma-separated/nested jump chains,
  interactive PTYs, encrypted private-key prompts, and remote cache cleanup are
  not implemented yet.
- Registry support covers anonymous and password-authenticated OCI pull,
  incremental push, and local OCI pack/unpack. Persistent login and custom CAs
  are not implemented yet.

## Security

- Official downloads are pinned by version and SHA-256.
- Server keys are checked against the default `known_hosts` file.
- Remote uploads use a restrictive umask, temporary file, and atomic rename.
- Tool arguments are passed as positional data rather than interpolated shell
  fragments.

Report vulnerabilities privately according to [SECURITY.md](SECURITY.md).

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the local
development checks and project guidelines. User-visible changes are tracked in
[CHANGELOG.md](CHANGELOG.md), and participation is governed by the
[Code of Conduct](CODE_OF_CONDUCT.md).

## License

MIT
