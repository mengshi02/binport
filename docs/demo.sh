#!/bin/sh
set -eu

# Deterministic, credential-free source for the README terminal recording.
# Every displayed result comes from the current release binary. Documentation
# address ranges are planned offline and are never contacted.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binport=${DEMO_BINPORT:-"$project_root/target/release/binport"}
demo_home=$(mktemp -d "${TMPDIR:-/tmp}/binport-demo.XXXXXX")
trap 'rm -rf "$demo_home"' EXIT INT TERM

prompt() {
  printf '\033[1;36m❯\033[0m %s\n' "$1"
  sleep 1
}

cd "$project_root"
"$binport" build . >/dev/null
printf '\033c'
printf '\033[1;35m%s\033[0m\n' 'Stop hand-writing ProxyJump config.'
printf '%s\n\n' 'One binary · standard SSH aliases · zero remote setup'
sleep 2

prompt 'binport host add jump root@203.0.113.10'
HOME="$demo_home" "$binport" host add jump root@203.0.113.10 |
  sed '/^Config:/d; /^$/d; /binport host test/d; /binport jump rg/d'
sleep 2

prompt 'binport host add app-01 root@10.0.0.52 --jump jump'
HOME="$demo_home" "$binport" host add app-01 root@10.0.0.52 --jump jump |
  sed '/^Config:/d; /^$/d; /binport host test/d; /binport app-01 rg/d'
sleep 2

prompt 'binport host ls'
HOME="$demo_home" "$binport" host ls
sleep 3

prompt 'binport plan app-01 rg'
HOME="$demo_home" "$binport" plan app-01 rg |
  sed -E 's#\$HOME/\.cache/binport/[0-9a-f]+/rg#$HOME/.cache/binport/<sha256>/rg#'
sleep 3

printf '\n\033[1;32m%s\033[0m\n' 'local → jump → app-01. Ready for every toolbox command.'
sleep 3
