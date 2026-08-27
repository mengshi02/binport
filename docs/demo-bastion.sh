#!/bin/sh
set -eu

# Real, credential-free README recording. It exercises binport's native
# application-layer bastion path against a disposable localhost sshd. No
# private addresses, production credentials, or external ssh/scp execution are
# involved in the recorded product commands.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
demo_root=$(mktemp -d "${TMPDIR:-/tmp}/binport-bastion-demo.XXXXXX")
port=$((33000 + ($$ % 10000)))
service_port=$((44000 + ($$ % 10000)))
local_port=$((55000 + ($$ % 5000)))
binport="$project_root/target/release/binport"
sshd_pid=
service_pid=
tunnel_pid=
agent_pid=

cleanup() {
  [ -z "$tunnel_pid" ] || kill "$tunnel_pid" 2>/dev/null || true
  [ -z "$service_pid" ] || kill "$service_pid" 2>/dev/null || true
  [ -z "$sshd_pid" ] || kill "$sshd_pid" 2>/dev/null || true
  [ -z "$agent_pid" ] || kill "$agent_pid" 2>/dev/null || true
  rm -rf "$demo_root"
}
trap cleanup EXIT INT TERM

prompt() {
  printf '\033[1;36m❯\033[0m %s\n' "$1"
  sleep 1
}

mkdir -p "$demo_root/home/.ssh" "$demo_root/remote/bin" "$demo_root/project"
ssh-keygen -q -t ed25519 -N '' -f "$demo_root/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$demo_root/client_key"
cp "$demo_root/client_key.pub" "$demo_root/authorized_keys"

"${RUSTC:-$HOME/.cargo/bin/rustc}" -C opt-level=2 \
  "$project_root/docs/demo-remote/rg.rs" -o "$demo_root/project/rg"
printf '%s\n' 'TARGET linux/amd64' 'COPY ./rg rg --target linux/amd64' \
  >"$demo_root/project/Binfile"

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
  'AllowTcpForwarding yes' \
  'LogLevel ERROR' >"$demo_root/sshd_config"

awk -v port="$port" '{ print "[127.0.0.1]:" port " " $1 " " $2 }' \
  "$demo_root/host_key.pub" >"$demo_root/home/.ssh/known_hosts"
printf '%s\n' \
  'Host worker-a' \
  '    HostName 192.0.2.52' \
  "    User $(id -un)" \
  '    BastionProxy 127.0.0.1' \
  '    BastionUser demo' \
  "    BastionAccount $(id -un)" \
  "    BastionPort $port" \
  '    BastionFormat {account}' >"$demo_root/home/.ssh/config"

/usr/sbin/sshd -D -f "$demo_root/sshd_config" -E "$demo_root/sshd.log" &
sshd_pid=$!
eval "$(ssh-agent -s)" >/dev/null
agent_pid=$SSH_AGENT_PID
ssh-add "$demo_root/client_key" >/dev/null 2>&1

cd "$demo_root/project"
"$binport" resolve . >/dev/null
"$binport" build . >/dev/null

mkdir -p "$demo_root/service"
printf '%s\n' 'private service reached through binport' >"$demo_root/service/index.html"
"${PYTHON:-python3}" -m http.server "$service_port" \
  --bind 127.0.0.1 --directory "$demo_root/service" \
  >"$demo_root/http.log" 2>&1 &
service_pid=$!

printf '\033c'
printf '\033[1;35m%s\033[0m\n' 'Your tools. Any server. Even behind a bastion.'
printf '%s\n\n' 'Native Rust SSH · zero remote installs · no external ssh/scp'
sleep 2

prompt 'binport bastion presets'
HOME="$demo_root/home" "$binport" bastion presets
sleep 2

prompt 'binport bastion probe worker-a'
HOME="$demo_root/home" SSH_AUTH_SOCK="$SSH_AUTH_SOCK" \
  "$binport" bastion probe worker-a |
  sed "s#127.0.0.1:$port#bastion.example.com:22#; /Preset:/d"
sleep 2

prompt 'binport --verbose worker-a rg "authentication timeout" /var/log'
HOME="$demo_root/home" SSH_AUTH_SOCK="$SSH_AUTH_SOCK" \
  "$binport" --verbose worker-a rg 'authentication timeout' /var/log 2>&1 |
  sed "s#$(id -un)@192.0.2.52#operator@192.0.2.52#; s#bastion:127.0.0.1#bastion:bastion.example.com#"
sleep 2

prompt 'binport --verbose worker-a rg "authentication timeout" /var/log'
HOME="$demo_root/home" SSH_AUTH_SOCK="$SSH_AUTH_SOCK" \
  "$binport" --verbose worker-a rg 'authentication timeout' /var/log 2>&1 |
  sed "s#$(id -un)@192.0.2.52#operator@192.0.2.52#; s#bastion:127.0.0.1#bastion:bastion.example.com#"
sleep 2

prompt 'binport tunnel 8080:127.0.0.1:3000 worker-a'
HOME="$demo_root/home" SSH_AUTH_SOCK="$SSH_AUTH_SOCK" \
  "$binport" tunnel "$local_port:127.0.0.1:$service_port" worker-a \
  >"$demo_root/tunnel.log" 2>&1 &
tunnel_pid=$!
sleep 2
sed -n '1p' "$demo_root/tunnel.log" | sed "s/$local_port/8080/g; s/$service_port/3000/g"

prompt 'curl -s http://127.0.0.1:8080'
curl -s "http://127.0.0.1:$local_port"
printf '\n\033[32m%s\033[0m\n' 'No binport agent. No package install. Just your tools.'
sleep 4
