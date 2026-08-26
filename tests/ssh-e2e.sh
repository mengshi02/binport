#!/bin/sh
set -eu

# Real localhost SSH integration test. All keys, ports, homes, and remote files
# are disposable; no network access or long-lived credentials are required.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/binport-ssh-e2e.XXXXXX")
port=$((32000 + ($$ % 20000)))
sshd_pid=
binport_bin=${BINPORT_BIN:-"$project_root/target/release/binport"}
rustc_bin=${RUSTC:-rustc}
sshd_bin=${SSHD_BIN:-$(command -v sshd 2>/dev/null || true)}
if [ -z "$sshd_bin" ] && [ -x /usr/sbin/sshd ]; then
  sshd_bin=/usr/sbin/sshd
fi

cleanup() {
  if [ -n "$sshd_pid" ]; then
    kill "$sshd_pid" 2>/dev/null || true
    wait "$sshd_pid" 2>/dev/null || true
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

mkdir -p \
  "$test_root/client/.ssh" \
  "$test_root/remote/bin" \
  "$test_root/project"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$test_root/client_key"
cp "$test_root/client_key.pub" "$test_root/authorized_keys"

"$rustc_bin" -C opt-level=1 "$project_root/docs/demo-remote/rg.rs" \
  -o "$test_root/project/rg"
printf '%s\n' \
  'TARGET linux/amd64' \
  'COPY ./rg rg --target linux/amd64' >"$test_root/project/Binfile"

# Keep platform detection deterministic on both amd64 and arm64 CI runners.
printf '%s\n' \
  '#!/bin/sh' \
  'if [ "${1:-}" = "-s" ]; then echo Linux' \
  'elif [ "${1:-}" = "-m" ]; then echo x86_64' \
  'else exec /usr/bin/uname "$@"' \
  'fi' >"$test_root/remote/bin/uname"
chmod +x "$test_root/remote/bin/uname"

printf '%s\n' \
  '#!/bin/sh' \
  "export HOME='$test_root/remote'" \
  "export PATH='$test_root/remote/bin:/usr/bin:/bin'" \
  "export BINPORT_DEMO_LOG='$project_root/docs/demo-remote/auth.log'" \
  'exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"' >"$test_root/force-command"
chmod +x "$test_root/force-command"

printf '%s\n' \
  "Port $port" \
  'ListenAddress 127.0.0.1' \
  "HostKey $test_root/host_key" \
  "PidFile $test_root/sshd.pid" \
  "AuthorizedKeysFile $test_root/authorized_keys" \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'PubkeyAuthentication yes' \
  'UsePAM no' \
  'StrictModes no' \
  "ForceCommand $test_root/force-command" \
  'LogLevel ERROR' >"$test_root/sshd_config"

awk -v port="$port" '{ print "[127.0.0.1]:" port " " $1 " " $2 }' \
  "$test_root/host_key.pub" >"$test_root/client/.ssh/known_hosts"
printf '%s\n' \
  'Host e2e-node' \
  '    HostName 127.0.0.1' \
  "    Port $port" \
  "    User $(id -un)" \
  "    IdentityFile $test_root/client_key" \
  "    UserKnownHostsFile $test_root/client/.ssh/known_hosts" \
  '    IdentitiesOnly yes' >"$test_root/client/.ssh/config"

"$sshd_bin" -D -f "$test_root/sshd_config" -E "$test_root/sshd.log" &
sshd_pid=$!

attempt=0
while ! HOME="$test_root/client" ssh -F "$test_root/client/.ssh/config" \
  -o BatchMode=yes e2e-node true >"$test_root/ready.log" 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    sed -n '1,120p' "$test_root/sshd.log" >&2
    sed -n '1,120p' "$test_root/ready.log" >&2
    fail "sshd did not become ready"
  fi
  sleep 0.1
done

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
printf '%s' "$doctor_output" | grep -q '1/1'
warm_output=$(HOME="$test_root/client" "$binport_bin" warm e2e-node)
printf '%s' "$warm_output" | grep -q 'UPLOADED'
watch_output=$(HOME="$test_root/client" "$binport_bin" watch --interval 1 --count 1 \
  e2e-node rg 'authentication timeout' /var/log/auth.log)
printf '%s' "$watch_output" | grep -q 'INITIAL'

printf '%s\n' 'round-trip over native SSH' >"$test_root/local.txt"
remote_path="$test_root/remote/copied.txt"
HOME="$test_root/client" "$binport_bin" cp \
  "$test_root/local.txt" "e2e-node:$remote_path" >/dev/null
HOME="$test_root/client" "$binport_bin" cp \
  "e2e-node:$remote_path" "$test_root/downloaded.txt" >/dev/null
cmp "$test_root/local.txt" "$test_root/downloaded.txt"
HOME="$test_root/client" "$binport_bin" rm "e2e-node:$remote_path" >/dev/null
test ! -e "$remote_path" || fail "remote file was not removed"

printf '%s\n' 'SSH E2E passed: execute, cache, fleet, copy, and remove'
