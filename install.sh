#!/bin/sh
set -eu

repository="${BINPORT_REPOSITORY:-mengshi02/binport}"
version="${BINPORT_VERSION:-latest}"

case "$(uname -s)" in
  Linux) platform="linux" ;;
  Darwin) platform="darwin" ;;
  *) echo "binport: unsupported operating system: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) architecture="amd64" ;;
  arm64|aarch64) architecture="arm64" ;;
  *) echo "binport: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

archive="binport-${platform}-${architecture}.tar.gz"
if [ -n "${BINPORT_RELEASE_URL:-}" ]; then
  release_url="${BINPORT_RELEASE_URL%/}"
elif [ "$version" = "latest" ]; then
  release_url="https://github.com/${repository}/releases/latest/download"
else
  release_url="https://github.com/${repository}/releases/download/${version}"
fi

temporary="$(mktemp -d "${TMPDIR:-/tmp}/binport-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

echo "binport: downloading ${archive}"
curl -fsSL "${release_url}/${archive}" -o "${temporary}/${archive}"
curl -fsSL "${release_url}/SHA256SUMS" -o "${temporary}/SHA256SUMS"

expected="$(awk -v archive="$archive" '$2 == archive { print $1 }' "${temporary}/SHA256SUMS")"
if [ -z "$expected" ]; then
  echo "binport: ${archive} is missing from SHA256SUMS" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${temporary}/${archive}" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "${temporary}/${archive}" | awk '{ print $1 }')"
else
  echo "binport: sha256sum or shasum is required" >&2
  exit 1
fi

if [ "$actual" != "$expected" ]; then
  echo "binport: checksum verification failed" >&2
  exit 1
fi

tar -xzf "${temporary}/${archive}" -C "$temporary"
binary="${temporary}/binport-${platform}-${architecture}/binport"

if [ -n "${BINPORT_INSTALL_DIR:-}" ]; then
  install_dir="$BINPORT_INSTALL_DIR"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  install_dir="/usr/local/bin"
else
  install_dir="${HOME}/.local/bin"
fi

mkdir -p "$install_dir"
install -m 755 "$binary" "$install_dir/binport"
echo "binport: installed to ${install_dir}/binport"

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) echo "binport: add ${install_dir} to PATH" ;;
esac
