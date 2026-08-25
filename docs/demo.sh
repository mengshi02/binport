#!/bin/sh
set -eu

# Deterministic, credential-free source for the README terminal recording.
# Product output is produced by the current release binary. Only absolute local
# paths and content hashes are shortened for a readable, machine-neutral demo.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binport=${DEMO_BINPORT:-"$project_root/target/release/binport"}
demo_home=$(mktemp -d "${TMPDIR:-/tmp}/binport-demo.XXXXXX")
trap 'rm -rf "$demo_home" "${TMPDIR:-/tmp}/binport-demo.oci"' EXIT INT TERM

mkdir -p "$demo_home/.ssh"
printf '%s\n' \
  'Host bastion' \
  '    HostName 203.0.113.10' \
  '    User ops' \
  '' \
  'Host prod-api-01' \
  '    HostName 192.0.2.15' \
  '    User deploy' \
  '    ProxyJump bastion' \
  '' \
  'Host prod-api-02' \
  '    HostName 192.0.2.16' \
  '    User deploy' \
  '    ProxyJump bastion' >"$demo_home/.ssh/config"

prompt() {
  printf '\033[1;36m❯\033[0m %s\n' "$1"
  sleep 1
}

clear
printf '\033[1;35m%s\033[0m\n' 'Build once. Run on any SSH host.'
printf '%s\n\n' 'No install · no container · no agent'
sleep 2

cd "$project_root"
prompt 'binport build .'
"$binport" build . | sed 's#^Manifest: .*#Manifest: .binport/toolbox.json#'
sleep 2

prompt 'binport plan @prod rg'
HOME="$demo_home" "$binport" plan @prod rg |
  sed -E 's#\$HOME/\.cache/binport/[0-9a-f]+/rg#$HOME/.cache/binport/<sha256>/rg#'
sleep 3

prompt 'binport pack /tmp/binport-demo.oci'
"$binport" pack "${TMPDIR:-/tmp}/binport-demo.oci" |
  sed 's#^Packed OCI toolbox into .*#Packed OCI toolbox into /tmp/binport-demo.oci#'
sleep 2

printf '\n\033[1;32m%s\033[0m\n' 'One toolbox. Any fleet. Zero remote setup.'
sleep 3
