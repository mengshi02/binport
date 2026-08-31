#!/bin/sh
set -eu
export LC_ALL=C

# Real localhost SSH integration test. All keys, ports, homes, and remote files
# are disposable; no network access or long-lived credentials are required.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/binport-ssh-e2e.XXXXXX")
entry_port=$((32000 + ($$ % 19000)))
target_port=$((entry_port + 1))
entry_sshd_pid=
target_sshd_pid=
http_pid=
release_http_pid=
tunnel_pid=
binport_bin=${BINPORT_BIN:-"$project_root/target/release/binport"}
binport_hop_bin=${BINPORT_HOP_BIN:-"$project_root/target/release/binport-hop"}
rustc_bin=${RUSTC:-rustc}
sshd_bin=${SSHD_BIN:-$(command -v sshd 2>/dev/null || true)}
if [ -z "$sshd_bin" ] && [ -x /usr/sbin/sshd ]; then
  sshd_bin=/usr/sbin/sshd
fi

cleanup() {
  for pid in "$entry_sshd_pid" "$target_sshd_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  done
  if [ -n "$tunnel_pid" ]; then
    kill "$tunnel_pid" 2>/dev/null || true
    wait "$tunnel_pid" 2>/dev/null || true
  fi
  if [ -n "$http_pid" ]; then
    kill "$http_pid" 2>/dev/null || true
    wait "$http_pid" 2>/dev/null || true
  fi
  if [ -n "$release_http_pid" ]; then
    kill "$release_http_pid" 2>/dev/null || true
    wait "$release_http_pid" 2>/dev/null || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

fail() {
  printf '%s\n' "SSH E2E failed: $*" >&2
  exit 1
}

test -n "$sshd_bin" || fail "sshd is required"
test -x "$binport_bin" || fail "binport binary not found at $binport_bin"
test -x "$binport_hop_bin" || fail "binport-hop binary not found at $binport_hop_bin"

mkdir -p \
  "$test_root/client/.ssh" \
  "$test_root/entry/.ssh" \
  "$test_root/entry/bin" \
  "$test_root/target/.ssh" \
  "$test_root/target/bin" \
  "$test_root/project"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/entry_host_key"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/target_host_key"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/client_key"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/target_key"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/rejected_target_key"
cp "$test_root/client_key.pub" "$test_root/entry_authorized_keys"
cp "$test_root/target_key.pub" "$test_root/target_authorized_keys"
cp "$test_root/rejected_target_key" "$test_root/entry/.ssh/id_ed25519"
cp "$test_root/target_key" "$test_root/entry/.ssh/id_rsa"
chmod 600 "$test_root/entry/.ssh/id_ed25519" "$test_root/entry/.ssh/id_rsa"

"$rustc_bin" -C opt-level=1 "$project_root/tests/fixtures/ssh-e2e/rg.rs" \
  -o "$test_root/project/rg"
printf '%s\n' \
  'TARGET linux/amd64' \
  'COPY ./rg rg --target linux/amd64' \
  'COPY ./rg btm --target linux/amd64' >"$test_root/project/Binfile"

# Keep platform detection deterministic on both amd64 and arm64 CI runners.
printf '%s\n' \
  '#!/bin/sh' \
  'if [ "${1:-}" = "-s" ]; then echo Linux' \
  'elif [ "${1:-}" = "-m" ]; then echo x86_64' \
  'else exec /usr/bin/uname "$@"' \
  'fi' >"$test_root/target/bin/uname"
chmod +x "$test_root/target/bin/uname"
cp "$test_root/target/bin/uname" "$test_root/entry/bin/uname"

printf '%s\n' \
  '#!/bin/sh' \
  "export HOME='$test_root/target'" \
  "export PATH='$test_root/target/bin:/usr/bin:/bin'" \
  "export BINPORT_E2E_LOG='$project_root/tests/fixtures/ssh-e2e/auth.log'" \
  'exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"' >"$test_root/target-force-command"
chmod +x "$test_root/target-force-command"
printf '%s\n' \
  '#!/bin/sh' \
  "export HOME='$test_root/entry'" \
  "export PATH='$test_root/entry/bin:/usr/bin:/bin'" \
  "export BINPORT_E2E_LOG='$project_root/tests/fixtures/ssh-e2e/auth.log'" \
  'exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"' >"$test_root/entry-force-command"
chmod +x "$test_root/entry-force-command"

printf '%s\n' \
  "Port $entry_port" \
  'ListenAddress 127.0.0.1' \
  "HostKey $test_root/entry_host_key" \
  "PidFile $test_root/entry-sshd.pid" \
  "AuthorizedKeysFile $test_root/entry_authorized_keys" \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'PubkeyAuthentication yes' \
  'UsePAM no' \
  'StrictModes no' \
  "ForceCommand $test_root/entry-force-command" \
  'LogLevel ERROR' >"$test_root/entry-sshd_config"

printf '%s\n' \
  "Port $target_port" \
  'ListenAddress 127.0.0.1' \
  "HostKey $test_root/target_host_key" \
  "PidFile $test_root/target-sshd.pid" \
  "AuthorizedKeysFile $test_root/target_authorized_keys" \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'PubkeyAuthentication yes' \
  'UsePAM no' \
  'StrictModes no' \
  "ForceCommand $test_root/target-force-command" \
  'LogLevel ERROR' >"$test_root/target-sshd_config"

awk -v port="$entry_port" '{ print "[127.0.0.1]:" port " " $1 " " $2 }' \
  "$test_root/entry_host_key.pub" >"$test_root/client/.ssh/known_hosts"
awk -v port="$target_port" '{ print "[127.0.0.1]:" port " " $1 " " $2 }' \
  "$test_root/target_host_key.pub" >"$test_root/entry/.ssh/known_hosts"
printf '%s\n' \
  'Host e2e-node' \
  '    HostName 127.0.0.1' \
  "    Port $entry_port" \
  "    User $(id -un)" \
  "    IdentityFile $test_root/client_key" \
  "    UserKnownHostsFile $test_root/client/.ssh/known_hosts" \
  '    IdentitiesOnly yes' >"$test_root/client/.ssh/config"

"$sshd_bin" -D -f "$test_root/entry-sshd_config" -E "$test_root/entry-sshd.log" &
entry_sshd_pid=$!
"$sshd_bin" -D -f "$test_root/target-sshd_config" -E "$test_root/target-sshd.log" &
target_sshd_pid=$!

attempt=0
while ! HOME="$test_root/client" ssh -F "$test_root/client/.ssh/config" \
  -o BatchMode=yes e2e-node true >"$test_root/ready.log" 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    sed -n '1,120p' "$test_root/entry-sshd.log" >&2
    sed -n '1,120p' "$test_root/ready.log" >&2
    fail "sshd did not become ready"
  fi
  sleep 0.1
done

# Prove the client credential cannot authenticate the target directly.
if ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes \
  -o StrictHostKeyChecking=yes \
  -o "UserKnownHostsFile=$test_root/entry/.ssh/known_hosts" \
  -i "$test_root/client_key" -p "$target_port" \
  "$(id -un)@127.0.0.1" true >/dev/null 2>&1; then
  fail "client key unexpectedly authenticated the isolated target"
fi

cd "$test_root/project"
HOME="$test_root/client" "$binport_bin" resolve . >/dev/null
HOME="$test_root/client" "$binport_bin" build . >/dev/null

HOME="$test_root/client" "$binport_bin" host test e2e-node >/dev/null
first_run=$(HOME="$test_root/client" "$binport_bin" e2e-node rg \
  'authentication timeout' /var/log/auth.log)
printf '%s' "$first_run" | grep -q 'authentication timeout upstream=identity' || \
  fail "remote command output was not returned"

second_run=$(HOME="$test_root/client" "$binport_bin" --verbose e2e-node rg \
  'authentication timeout' /var/log/auth.log 2>&1)
printf '%s' "$second_run" | grep -q 'cache hit' || fail "second execution missed cache"

plan_output=$(HOME="$test_root/client" "$binport_bin" plan e2e-node rg)
printf '%s' "$plan_output" | grep -q 'Plan only'
doctor_output=$(HOME="$test_root/client" "$binport_bin" doctor e2e-node)
printf '%s' "$doctor_output" | grep -q '1/2'
warm_output=$(HOME="$test_root/client" "$binport_bin" warm e2e-node)
printf '%s' "$warm_output" | grep -q 'UPLOADED'
watch_output=$(HOME="$test_root/client" "$binport_bin" watch --interval 1 --count 1 \
  e2e-node rg 'authentication timeout' /var/log/auth.log)
printf '%s' "$watch_output" | grep -q 'INITIAL'

printf '%s\n' 'round-trip over native SSH' >"$test_root/local.txt"
remote_path="$test_root/entry/copied.txt"
HOME="$test_root/client" "$binport_bin" cp \
  "$test_root/local.txt" "e2e-node:$remote_path" >/dev/null
HOME="$test_root/client" "$binport_bin" cp \
  "e2e-node:$remote_path" "$test_root/downloaded.txt" >/dev/null
cmp "$test_root/local.txt" "$test_root/downloaded.txt"
HOME="$test_root/client" "$binport_bin" rm "e2e-node:$remote_path" >/dev/null
test ! -e "$remote_path" || fail "remote file was not removed"

# Exercise the complete guided setup path. The client key is authorized only
# on entry; target accepts a different key held only in entry's HOME.
guided_output=$(printf '%s\n' \
  "$(id -un)@127.0.0.1" 2 e2e-node y y y | \
  HOME="$test_root/client" "$binport_bin" host add e2e-hop --port "$target_port")
printf '%s' "$guided_output" | grep -q 'Strategy: exec-hop' || \
  fail "guided setup did not select exec-hop"
grep -q 'BinportStrategy exec-hop' "$test_root/client/.ssh/binport_config" || \
  fail "guided setup did not persist exec-hop"
release_port=$((target_port + 3))
package=binport-linux-amd64
mkdir -p "$test_root/release/$package"
cp "$binport_hop_bin" "$test_root/release/$package/binport-hop"
(cd "$test_root/release" && tar -czf "$package.tar.gz" "$package")
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$test_root/release" && sha256sum "$package.tar.gz" >SHA256SUMS)
else
  checksum=$(shasum -a 256 "$test_root/release/$package.tar.gz" | awk '{print $1}')
  printf '%s  %s\n' "$checksum" "$package.tar.gz" >"$test_root/release/SHA256SUMS"
fi
python3 -m http.server "$release_port" --bind 127.0.0.1 \
  --directory "$test_root/release" >"$test_root/release-http.log" 2>&1 &
release_http_pid=$!
attempt=0
while ! curl --fail --silent --max-time 1 \
  "http://127.0.0.1:$release_port/SHA256SUMS" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then fail "local helper release mirror did not start"; fi
  sleep 0.1
done
hop_env="BINPORT_RELEASE_BASE=http://127.0.0.1:$release_port"
hop_output=$(env HOME="$test_root/client" "$hop_env" "$binport_bin" e2e-hop rg \
  'authentication timeout' /var/log/auth.log)
printf '%s' "$hop_output" | grep -q 'authentication timeout upstream=identity' || \
  fail "exec-hop command output was not returned"

set +e
if [ "$(uname -s)" = Linux ]; then
  tty_output=$(timeout 5s script -q -e -c \
    "exec env HOME='$test_root/client' '$hop_env' '$binport_bin' e2e-hop btm 'authentication timeout' /var/log/auth.log" \
    /dev/null 2>&1)
else
  tty_output=$({ sleep 2; printf q; } | script -q /dev/null env HOME="$test_root/client" "$hop_env" \
    "$binport_bin" e2e-hop btm 'authentication timeout' /var/log/auth.log 2>&1)
fi
tty_status=$?
set -e
if [ "$tty_status" -ne 0 ] && [ "$tty_status" -ne 124 ]; then
  fail "exec-hop TTY exited $tty_status: $tty_output"
fi
printf '%s' "$tty_output" | grep -q 'TTY_READY' || \
  fail "exec-hop TTY startup output was not returned"

printf '%s\n' 'round-trip over native exec-hop' >"$test_root/hop-local.txt"
hop_remote="$test_root/target/hop-copied.txt"
env HOME="$test_root/client" "$hop_env" "$binport_bin" cp \
  "$test_root/hop-local.txt" "e2e-hop:$hop_remote" >/dev/null
env HOME="$test_root/client" "$hop_env" "$binport_bin" cp \
  "e2e-hop:$hop_remote" "$test_root/hop-downloaded.txt" >/dev/null
cmp "$test_root/hop-local.txt" "$test_root/hop-downloaded.txt"
env HOME="$test_root/client" "$hop_env" "$binport_bin" rm \
  "e2e-hop:$hop_remote" >/dev/null
test ! -e "$hop_remote" || fail "exec-hop remote file was not removed"

http_port=$((target_port + 1))
local_tunnel_port=$((target_port + 2))
python3 -m http.server "$http_port" --bind 127.0.0.1 \
  --directory "$test_root/target" >"$test_root/http.log" 2>&1 &
http_pid=$!
env HOME="$test_root/client" "$hop_env" "$binport_bin" tunnel \
  "$local_tunnel_port:127.0.0.1:$http_port" e2e-hop \
  >"$test_root/tunnel.log" 2>&1 &
tunnel_pid=$!
attempt=0
while ! curl --fail --silent --max-time 1 "http://127.0.0.1:$local_tunnel_port/" \
  >"$test_root/tunnel-response.html"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 40 ]; then
    sed -n '1,160p' "$test_root/tunnel.log" >&2
    fail "exec-hop TCP relay did not become ready"
  fi
  sleep 0.1
done
grep -q 'Directory listing' "$test_root/tunnel-response.html" || \
  fail "exec-hop TCP relay returned an unexpected response"

# An entry authentication failure must identify the entry, not the target alias.
mv "$test_root/client_key" "$test_root/client_key.disabled"
if auth_error=$(env HOME="$test_root/client" "$hop_env" "$binport_bin" \
  e2e-hop rg --version 2>&1); then
  fail "exec-hop unexpectedly worked without its entry credential"
fi
mv "$test_root/client_key.disabled" "$test_root/client_key"
printf '%s' "$auth_error" | grep -q 'auth setup e2e-node' || \
  fail "authentication hint did not identify the entry host: $auth_error"

printf '%s\n' 'SSH E2E passed: native and exec-hop execute, copy, remove, and TCP relay'
