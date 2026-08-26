#!/bin/sh
set -eu

# Launch the isolated interactive shell used by the README recording. The
# recorder types commands into this shell; this file never contains credentials.
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
demo_home=$(mktemp -d "${TMPDIR:-/tmp}/binport-demo-home.XXXXXX")
cache_home=${XDG_CACHE_HOME:-"$HOME/.cache"}
trap 'rm -rf "$demo_home"' EXIT INT TERM

mkdir -p "$demo_home/.ssh"

cd "$project_root"
HOME="$demo_home" \
XDG_CACHE_HOME="$cache_home" \
PATH="$project_root/target/release:$PATH" \
PS1='❯ ' \
  /bin/zsh -df
