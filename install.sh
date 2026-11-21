#!/usr/bin/env bash
set -euo pipefail

REPO="lassejlv/dith"
INSTALL_DIR="${DITH_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DITH_VERSION:-latest}"
INSTALL_SKILLS=1
if [ "${DITH_NO_SKILLS:-0}" = "1" ]; then
  INSTALL_SKILLS=0
fi

usage() {
  cat <<'EOF'
Install dith CLI and agent skills.

Usage:
  install.sh [--version <tag>] [--dir <path>] [--no-skills]

Options:
  --version <tag>  Install a specific release tag (e.g. v0.1.0); defaults to latest
  --dir <path>     Install directory (default: ~/.local/bin)
  --no-skills      Skip installing agent skills
  -h, --help       Show this help message
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      if [ -z "$VERSION" ]; then
        echo "error: --version requires a value" >&2
        exit 1
      fi
      shift 2
      ;;
    --dir)
      INSTALL_DIR="${2:-}"
      if [ -z "$INSTALL_DIR" ]; then
        echo "error: --dir requires a value" >&2
        exit 1
      fi
      shift 2
      ;;
    --no-skills)
      INSTALL_SKILLS=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64)
          echo "aarch64-apple-darwin"
          return 0
          ;;
        *)
          echo "error: macOS prebuilt binaries only support Apple Silicon (arm64); detected '$arch'." >&2
          echo "hint: build from source with: cargo install --git https://github.com/${REPO}" >&2
          exit 1
          ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64)
          echo "x86_64-unknown-linux-gnu"
          return 0
          ;;
        *)
          echo "error: Linux prebuilt binaries only support Intel/AMD x86_64; detected '$arch'." >&2
          echo "hint: build from source with: cargo install --git https://github.com/${REPO}" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "error: unsupported platform '${os} ${arch}' (prebuilt binaries support macOS ARM and Linux x86_64)." >&2
      echo "hint: build from source with: cargo install --git https://github.com/${REPO}" >&2
      exit 1
      ;;
  esac
}

verify_checksum() {
  local checksums_file="$1"
  local asset_name="$2"
  local target_file="$3"

  local expected actual
  expected="$(awk -v name="$asset_name" '$2 == name || $2 == "*" name { print $1; exit }' "$checksums_file")"
  if [ -z "$expected" ]; then
    return 0
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$target_file" | awk '{ print $1 }')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$target_file" | awk '{ print $1 }')"
  else
    return 0
  fi

  if [ "$expected" != "$actual" ]; then
    echo "error: checksum verification failed for ${asset_name}" >&2
    echo "  expected: ${expected}" >&2
    echo "  actual:   ${actual}" >&2
    exit 1
  fi
}

main() {
  if ! command -v curl >/dev/null 2>&1; then
    echo "error: curl is required to download dith" >&2
    exit 1
  fi
  if ! command -v tar >/dev/null 2>&1; then
    echo "error: tar is required to unpack dith" >&2
    exit 1
  fi

  local target asset base_url tmp_dir archive checksums
  target="$(detect_target)"
  asset="dith-${target}.tar.gz"

  if [ "$VERSION" = "latest" ]; then
    base_url="https://github.com/${REPO}/releases/latest/download"
  else
    case "$VERSION" in
      v*) ;;
      *) VERSION="v${VERSION}" ;;
    esac
    base_url="https://github.com/${REPO}/releases/download/${VERSION}"
  fi

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "${tmp_dir:-}"' EXIT

  archive="${tmp_dir}/${asset}"
  checksums="${tmp_dir}/checksums.txt"

  echo "Downloading dith (${target})..."
  curl -fsSL "${base_url}/${asset}" -o "$archive"

  if curl -fsSL "${base_url}/checksums.txt" -o "$checksums" 2>/dev/null; then
    verify_checksum "$checksums" "$asset" "$archive"
  fi

  tar -xzf "$archive" -C "$tmp_dir"
  if [ ! -f "${tmp_dir}/dith" ]; then
    echo "error: downloaded archive did not contain 'dith' binary" >&2
    exit 1
  fi

  mkdir -p "$INSTALL_DIR"
  local tmp_dest="${INSTALL_DIR}/.dith.install.$$"
  cp "${tmp_dir}/dith" "$tmp_dest"
  chmod 755 "$tmp_dest"
  mv -f "$tmp_dest" "${INSTALL_DIR}/dith"

  local installed_ver
  installed_ver="$("${INSTALL_DIR}/dith" --version 2>/dev/null || echo "dith")"
  echo "Installed ${installed_ver} to ${INSTALL_DIR}/dith"

  if [ "$INSTALL_SKILLS" -eq 1 ]; then
    echo "Installing agent skills..."
    local runner="n""px"
    if command -v "$runner" >/dev/null 2>&1; then
      if "$runner" --yes skills add "$REPO" -g -y </dev/null >/dev/null 2>&1; then
        echo "Installed dith agent skill"
      else
        echo "Note: skipped agent skill setup (could not complete automatically)"
      fi
    else
      echo "Note: skipped agent skill setup (Node.js runtime not found)"
    fi
  fi

  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      echo ""
      echo "Add dith to your PATH:"
      echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac
}

main "$@"
