#!/usr/bin/env bash
set -euo pipefail

REPO="lassejlv/diths"
INSTALL_DIR="${DITH_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DITH_VERSION:-latest}"
INSTALL_SKILLS=1
if [ "${DITH_NO_SKILLS:-0}" = "1" ]; then
  INSTALL_SKILLS=0
fi
ASSUME_YES=0
if [ "${DITH_YES:-0}" = "1" ]; then
  ASSUME_YES=1
fi

usage() {
  cat <<'EOF'
Install dith CLI and agent skills.

Usage:
  install.sh [--version <tag>] [--dir <path>] [--no-skills] [--yes]

Options:
  --version <tag>  Install a specific release tag (e.g. v0.1.0); defaults to latest
  --dir <path>     Install directory (default: ~/.local/bin)
  --no-skills      Skip installing agent skills
  -y, --yes        Install agent skills into every detected agent without asking
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
    -y|--yes)
      ASSUME_YES=1
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
        aarch64|arm64)
          echo "aarch64-unknown-linux-gnu"
          return 0
          ;;
        *)
          echo "error: Linux prebuilt binaries only support x86_64 and aarch64; detected '$arch'." >&2
          echo "hint: build from source with: cargo install --git https://github.com/${REPO}" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "error: unsupported platform '${os} ${arch}' (prebuilt binaries support macOS ARM, Linux x86_64, and Linux aarch64)." >&2
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

# Agents that load skills from a global directory. A skill is only installed
# for an agent whose config directory already exists.
add_agent() {
  # $1 = display name, $2 = agent config dir, $3 = skills dir
  if [ -d "$2" ]; then
    AGENT_NAMES+=("$1")
    AGENT_DIRS+=("$3")
  fi
}

detect_agents() {
  AGENT_NAMES=()
  AGENT_DIRS=()
  local claude_home="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
  local codex_home="${CODEX_HOME:-$HOME/.codex}"
  local xdg_config="${XDG_CONFIG_HOME:-$HOME/.config}"

  add_agent "Claude Code"    "$claude_home"              "$claude_home/skills"
  add_agent "Codex"          "$codex_home"               "$codex_home/skills"
  add_agent "Cursor"         "$HOME/.cursor"             "$HOME/.cursor/skills"
  add_agent "Gemini CLI"     "$HOME/.gemini"             "$HOME/.gemini/skills"
  add_agent "OpenCode"       "$xdg_config/opencode"      "$xdg_config/opencode/skills"
  add_agent "GitHub Copilot" "$HOME/.copilot"            "$HOME/.copilot/skills"
  add_agent "Windsurf"       "$HOME/.codeium/windsurf"   "$HOME/.codeium/windsurf/skills"
  add_agent "Factory Droid"  "$HOME/.factory"            "$HOME/.factory/skills"
  add_agent "Shared agents"  "$HOME/.agents"             "$HOME/.agents/skills"
}

# Ask a yes/no question on the terminal. Reads from /dev/tty so it works when
# the script itself is piped in (curl ... | bash). Without a terminal the
# answer is "no" unless --yes was given.
confirm() {
  if [ "$ASSUME_YES" -eq 1 ]; then
    return 0
  fi
  local reply=""
  printf '%s [y/N] ' "$1" 2>/dev/null >/dev/tty || return 1
  read -r reply 2>/dev/null </dev/tty || return 1
  case "$reply" in
    y|Y|yes|Yes|YES) return 0 ;;
    *) return 1 ;;
  esac
}

# Download the repository source for the installed release and print the path
# of its skills/ directory.
fetch_skills() {
  local work_dir="$1"
  local ref="" url
  if [ "$VERSION" = "latest" ]; then
    ref="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
    ref="${ref##*/}"
    case "$ref" in
      v*) ;;
      *) ref="" ;;
    esac
  else
    ref="$VERSION"
  fi

  if [ -n "$ref" ]; then
    url="https://github.com/${REPO}/archive/refs/tags/${ref}.tar.gz"
  else
    url="https://github.com/${REPO}/archive/HEAD.tar.gz"
  fi

  mkdir -p "${work_dir}/src"
  curl -fsSL "$url" -o "${work_dir}/source.tar.gz" || return 1
  tar -xzf "${work_dir}/source.tar.gz" -C "${work_dir}/src" || return 1

  local dir
  for dir in "${work_dir}"/src/*/skills; do
    if [ -d "$dir" ]; then
      echo "$dir"
      return 0
    fi
  done
  return 1
}

install_skills() {
  local work_dir="$1"

  detect_agents
  if [ "${#AGENT_NAMES[@]}" -eq 0 ]; then
    echo "Note: no supported coding agents found; skipped agent skill setup"
    return 0
  fi

  echo ""
  echo "Found these coding agents:"
  local i
  for i in "${!AGENT_NAMES[@]}"; do
    printf '  %-15s %s\n' "${AGENT_NAMES[$i]}" "${AGENT_DIRS[$i]}/dith"
  done

  if ! confirm "Install the dith agent skill for these agents?"; then
    echo "Skipped agent skill setup (run with --yes to install without asking)"
    return 0
  fi

  local skills_src
  if ! skills_src="$(fetch_skills "$work_dir")"; then
    echo "Note: skipped agent skill setup (could not download skills)"
    return 0
  fi

  local skill name dest
  for i in "${!AGENT_DIRS[@]}"; do
    for skill in "$skills_src"/*/; do
      [ -f "${skill}SKILL.md" ] || continue
      name="$(basename "$skill")"
      dest="${AGENT_DIRS[$i]}/${name}"
      if mkdir -p "${AGENT_DIRS[$i]}" && rm -rf "$dest" && cp -R "${skill%/}" "$dest"; then
        echo "Installed ${name} skill for ${AGENT_NAMES[$i]}"
      else
        echo "Note: could not install ${name} skill for ${AGENT_NAMES[$i]} (${dest})"
      fi
    done
  done
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
    install_skills "$tmp_dir"
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
