#!/bin/sh
set -eu

# Disposable localhost SSH endpoint for recording a real binport transfer and
# execution without using private infrastructure or credentials.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
demo_root=$(mktemp -d "${TMPDIR:-/tmp}/binport-ssh-demo.XXXXXX")
port=32222
sshd_pid=
rustc=${RUSTC:-"$HOME/.cargo/bin/rustc"}

cleanup() {
  if [ -n "$sshd_pid" ]; then
    kill "$sshd_pid" 2>/dev/null || true
    wait "$sshd_pid" 2>/dev/null || true
  fi
  rm -rf "$demo_root"
}
trap cleanup EXIT INT TERM

mkdir -p "$demo_root/home/.ssh" "$demo_root/home/bin" "$demo_root/remote/bin" "$demo_root/project"
ssh-keygen -q -t ed25519 -N '' -f "$demo_root/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$demo_root/client_key"
cp "$demo_root/client_key.pub" "$demo_root/authorized_keys"

"$rustc" -C opt-level=2 "$project_root/docs/demo-remote/rg.rs" \
  -o "$demo_root/project/rg"
printf '%s\n' \
  'TARGET linux/amd64' \
  'COPY ./rg rg --target linux/amd64' >"$demo_root/project/Binfile"

printf '%s\n' \
  '#!/bin/sh' \
  'if [ "${1:-}" = "-s" ]; then echo Linux' \
  'elif [ "${1:-}" = "-m" ]; then echo x86_64' \
  'else exec /usr/bin/uname "$@"' \
  'fi' >"$demo_root/remote/bin/uname"
chmod +x "$demo_root/remote/bin/uname"

printf '%s\n' \
  '#!/bin/sh' \
  "export HOME='$demo_root/remote'" \
  "export PATH='$demo_root/remote/bin:/usr/bin:/bin'" \
  "export BINPORT_DEMO_LOG='$project_root/docs/demo-remote/auth.log'" \
  'exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"' >"$demo_root/force-command"
chmod +x "$demo_root/force-command"

printf '%s\n' \
  "Port $port" \
  'ListenAddress 127.0.0.1' \
  "HostKey $demo_root/host_key" \
  "PidFile $demo_root/sshd.pid" \
  "AuthorizedKeysFile $demo_root/authorized_keys" \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'PubkeyAuthentication yes' \
  'UsePAM no' \
  'StrictModes no' \
  "ForceCommand $demo_root/force-command" \
  'LogLevel ERROR' >"$demo_root/sshd_config"

awk -v port="$port" '{ print "[127.0.0.1]:" port " " $1 " " $2 }' \
  "$demo_root/host_key.pub" >"$demo_root/home/.ssh/known_hosts"
printf '%s\n' \
  'Host demo-node' \
  '    HostName 127.0.0.1' \
  "    Port $port" \
  "    User $(id -un)" \
  "    IdentityFile $demo_root/client_key" \
  '    IdentitiesOnly yes' >"$demo_root/home/.ssh/config"

printf '%s\n' \
  '#!/bin/sh' \
  'exec /usr/bin/ssh -F "$HOME/.ssh/config" -o UserKnownHostsFile="$HOME/.ssh/known_hosts" "$@"' >"$demo_root/home/bin/ssh"
chmod +x "$demo_root/home/bin/ssh"

/usr/sbin/sshd -D -f "$demo_root/sshd_config" -E "$demo_root/sshd.log" &
sshd_pid=$!
sleep 1

cd "$demo_root/project"
"$project_root/target/release/binport" resolve . >/dev/null
"$project_root/target/release/binport" build . >/dev/null

HOME="$demo_root/home" \
PATH="$demo_root/home/bin:$project_root/target/release:$PATH" \
PS1='❯ ' \
  /bin/zsh -df
