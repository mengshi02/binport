# Demo recording

The README animation is an interactive terminal recording of real binport
commands with documentation IP ranges and a temporary SSH configuration. It
requires no credentials or remote infrastructure.

Build the release binary and smoke-test the deterministic Host/ProxyJump
transcript:

```sh
cargo build --release --locked
sh docs/demo.sh
```

To record the animation with [termtosvg](https://github.com/nbedos/termtosvg),
launch `sh docs/demo-shell.sh` inside a `termtosvg record` session and type the
commands from `demo.sh`. Render the resulting cast with:

```sh
termtosvg render demo.cast docs/demo.svg \
  --template window_frame --loop-delay 2000 --max-frame-duration 2000
```

Do not add credentials or real infrastructure addresses to either recording
script.

For the end-to-end remote execution recording, `demo-remote/shell.sh` starts a
disposable localhost OpenSSH endpoint with ephemeral host and client keys. It
exercises the real SSH upload, remote cache, and execution path without private
infrastructure.
