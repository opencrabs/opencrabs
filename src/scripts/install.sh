#!/usr/bin/env bash
set -euo pipefail

# OpenCrabs one-line installer
#
#   curl -fsSL https://raw.githubusercontent.com/opencrabs/opencrabs/main/src/scripts/install.sh | bash
#
# Built to be safe to run unattended. It never prompts, never waits on a
# password, verifies the download against the release's SHA256SUMS, and exits
# non-zero with a message naming the fix when something is missing.
#
# Options:
#   --dir <path>      Install the CLI into <path> (default: $INSTALL_DIR, else /usr/local/bin)
#   --version <tag>   Install a specific release (default: the latest)
#   --dry-run         Resolve and print the plan, install nothing
#   --no-deps         Leave the system package manager alone
#   --no-path         Do not add the install dir to your shell startup file
#   -h, --help        Show this help
#
# Two archive layouts exist and both are handled: releases up to v0.5.11 ship a
# loose `opencrabs` plus `rtk`, and later releases ship `OpenCrabs.app` on
# macOS so the icon has a bundle to live in.
#
# Exit codes: 0 installed or dry run, 1 any failure.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}🦀${NC} $*"; }
warn() { echo -e "${YELLOW}⚠️${NC}  $*" >&2; }
error() {
  echo -e "${RED}❌${NC} $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
OpenCrabs one-line installer

  curl -fsSL https://raw.githubusercontent.com/opencrabs/opencrabs/main/src/scripts/install.sh | bash

Safe to run unattended: it never prompts, never waits on a password, and
verifies the download against the release's SHA256SUMS before unpacking.

Options:
  --dir <path>      Install the CLI into <path> (default: $INSTALL_DIR, else /usr/local/bin)
  --version <tag>   Install a specific release (default: the latest)
  --dry-run         Resolve and print the plan, install nothing
  --no-deps         Leave the system package manager alone
  --no-path         Do not add the install dir to your shell startup file
  -h, --help        Show this help

Two archive layouts exist and both are handled: releases up to v0.5.11 ship a
loose opencrabs plus rtk, and later releases ship OpenCrabs.app on macOS so
the icon has a bundle to live in.

Exit codes: 0 installed or dry run, 1 any failure.
EOF
}

# Retries: a transient 502 should not fail an unattended install.
fetch() { curl -fsSL --retry 3 --retry-delay 2 "$@"; }

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
APP_DIR="${OPENCRABS_APP_DIR:-$HOME/Applications}"
REQUESTED_VERSION="${OPENCRABS_VERSION:-}"
DRY_RUN=0
WITH_DEPS=1
WITH_PATH_ENTRY=1

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)
      [ -n "${2:-}" ] || error "--dir needs a path"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --dir=*)
      INSTALL_DIR="${1#*=}"
      shift
      ;;
    --version)
      [ -n "${2:-}" ] || error "--version needs a tag, for example v0.5.11"
      REQUESTED_VERSION="$2"
      shift 2
      ;;
    --version=*)
      REQUESTED_VERSION="${1#*=}"
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --no-deps)
      WITH_DEPS=0
      shift
      ;;
    --no-path)
      WITH_PATH_ENTRY=0
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      error "Unknown option: $1
   Run with --help to see what this installer accepts."
      ;;
  esac
done

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$ARCH" in
  x86_64 | amd64) ARCH="amd64" ;;
  aarch64 | arm64) ARCH="arm64" ;;
  *) error "Unsupported architecture: $ARCH (amd64 and arm64 only)" ;;
esac

case "$OS" in
  linux) ;;
  darwin) OS="macos" ;;
  *) error "Unsupported OS: $OS (linux and macos only)" ;;
esac

# Resolve the tag. A pinned version is taken as given; otherwise ask the API,
# which can rate limit, so the failure message names the way around it.
if [ -n "$REQUESTED_VERSION" ]; then
  case "$REQUESTED_VERSION" in
    v*) TAG="$REQUESTED_VERSION" ;;
    *) TAG="v$REQUESTED_VERSION" ;;
  esac
else
  info "Resolving the latest release..."
  TAG=$(fetch https://api.github.com/repos/opencrabs/opencrabs/releases/latest |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1) || TAG=""
  [ -n "$TAG" ] || error "Could not resolve the latest release tag.
   GitHub may be rate limiting this IP. Pass --version <tag> to pin one."
fi

FILENAME="opencrabs-${TAG}-${OS}-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/opencrabs/opencrabs/releases/download/${TAG}/${FILENAME}"

# Work out how the CLI path gets written. A missing directory is judged by its
# parent: --dir "$HOME/.local/bin" needs no root.
if [ -d "$INSTALL_DIR" ]; then
  PROBE="$INSTALL_DIR"
else
  PROBE=$(dirname "$INSTALL_DIR")
fi

# Whether we may prompt for a password. A human at a terminal gets the usual
# sudo prompt (it reads /dev/tty, so it still works when stdin is the curl
# pipe). An agent or CI job has no terminal, and waiting on a password there
# is a hang with no timeout, so it fails fast with the fix instead.
INTERACTIVE=0
if [ -t 2 ] && [ -r /dev/tty ]; then
  INTERACTIVE=1
fi

NEEDS_SUDO=0
if [ ! -w "$PROBE" ]; then
  NEEDS_SUDO=1
  if [ "$INTERACTIVE" -eq 1 ]; then
    info "Installing to ${INSTALL_DIR} needs sudo."
  elif ! sudo -n true 2>/dev/null; then
    error "Cannot write to ${INSTALL_DIR}, and passwordless sudo is not available.
   Pick a writable target instead:
     --dir \"\$HOME/.local/bin\"
   Nothing is prompting for a password here on purpose: an unattended run
   would hang forever waiting for one."
  fi
fi

as_root() {
  if [ "$NEEDS_SUDO" -eq 1 ]; then
    if [ "$INTERACTIVE" -eq 1 ]; then
      sudo "$@"
    else
      sudo -n "$@"
    fi
  else
    "$@"
  fi
}

if [ "$DRY_RUN" -eq 1 ]; then
  info "Dry run: would install ${TAG} for ${OS}-${ARCH}"
  info "Dry run: CLI goes to ${INSTALL_DIR}$([ "$NEEDS_SUDO" -eq 1 ] && echo " (via sudo -n)")"
  [ "$OS" = "macos" ] && info "Dry run: a bundled release would also land OpenCrabs.app in ${APP_DIR}"
  [ "$WITH_DEPS" -eq 1 ] && info "Dry run: would also make sure poppler (pdftoppm) is present"
  [ "$WITH_PATH_ENTRY" -eq 1 ] &&
    info "Dry run: would add ${INSTALL_DIR} to your shell startup file if it is not already on PATH"
  exit 0
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

info "Downloading ${TAG} for ${OS}-${ARCH}..."
fetch "$DOWNLOAD_URL" -o "${TMPDIR}/${FILENAME}" ||
  error "Failed to download ${FILENAME}
   URL: ${DOWNLOAD_URL}
   Check https://github.com/opencrabs/opencrabs/releases for available releases."

# Verify before anything is unpacked. Every release publishes SHA256SUMS, so a
# missing or mismatched checksum is a hard failure, not a warning.
info "Verifying checksum..."
fetch "https://github.com/opencrabs/opencrabs/releases/download/${TAG}/SHA256SUMS" \
  -o "${TMPDIR}/SHA256SUMS" ||
  error "Could not download SHA256SUMS for ${TAG}, so the download cannot be verified."

EXPECTED=$(awk -v f="$FILENAME" '$2 == f { print $1 }' "${TMPDIR}/SHA256SUMS")
[ -n "$EXPECTED" ] ||
  error "SHA256SUMS for ${TAG} does not list ${FILENAME}, so the download cannot be verified."

if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "${TMPDIR}/${FILENAME}" | awk '{print $1}')
else
  ACTUAL=$(shasum -a 256 "${TMPDIR}/${FILENAME}" | awk '{print $1}')
fi

[ "$ACTUAL" = "$EXPECTED" ] ||
  error "Checksum mismatch for ${FILENAME}
   expected: ${EXPECTED}
   actual:   ${ACTUAL}
   The download was corrupted or tampered with. Nothing was installed."

info "Extracting..."
tar xzf "${TMPDIR}/${FILENAME}" -C "$TMPDIR"

as_root mkdir -p "$INSTALL_DIR"

if [ -d "${TMPDIR}/OpenCrabs.app" ]; then
  # Bundled layout: macOS reads an app icon only from a bundle, so the archive
  # carries one and the CLI on PATH is a symlink into it.
  [ -x "${TMPDIR}/OpenCrabs.app/Contents/MacOS/opencrabs" ] ||
    error "OpenCrabs.app is in the archive but has no opencrabs executable inside."
  as_root mkdir -p "$APP_DIR"
  as_root rm -rf "${APP_DIR}/OpenCrabs.app"
  as_root cp -R "${TMPDIR}/OpenCrabs.app" "${APP_DIR}/OpenCrabs.app"
  as_root ln -sf "${APP_DIR}/OpenCrabs.app/Contents/MacOS/opencrabs" "${INSTALL_DIR}/opencrabs"
  info "Installed OpenCrabs.app in ${APP_DIR}, CLI linked into ${INSTALL_DIR}"
else
  [ -f "${TMPDIR}/opencrabs" ] ||
    error "The archive contained neither OpenCrabs.app nor an opencrabs binary, so the release layout changed."
  as_root install -m 755 "${TMPDIR}/opencrabs" "${INSTALL_DIR}/opencrabs"
  info "OpenCrabs ${TAG} installed to ${INSTALL_DIR}/opencrabs"
fi

# The archive ships rtk beside the agent. Installing only one leaves a feature
# promising a helper that is not there.
if [ -f "${TMPDIR}/rtk" ]; then
  as_root install -m 755 "${TMPDIR}/rtk" "${INSTALL_DIR}/rtk"
  info "rtk installed to ${INSTALL_DIR}/rtk"
fi

# A Linux desktop needs the menu entry and icon or a file manager shows the
# generic exec glyph. Best effort: a headless host has no use for them.
if [ -f "${TMPDIR}/opencrabs.desktop" ]; then
  mkdir -p "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/256x256/apps"
  install -m 644 "${TMPDIR}/opencrabs.desktop" "$HOME/.local/share/applications/opencrabs.desktop"
  [ -f "${TMPDIR}/opencrabs.png" ] &&
    install -m 644 "${TMPDIR}/opencrabs.png" \
      "$HOME/.local/share/icons/hicolor/256x256/apps/opencrabs.png"
fi

# Poppler (pdftoppm) renders PDF pages to images so the agent can see scanned
# PDFs. Best effort and opt-out: it mutates the system, which is not something
# an unattended caller should be surprised by.
install_poppler() {
  if command -v pdftoppm >/dev/null 2>&1; then
    info "poppler (pdftoppm) already installed, PDF rendering ready"
    return
  fi
  info "Installing poppler (pdftoppm) for PDF page rendering..."
  if command -v brew >/dev/null 2>&1; then
    brew install poppler || warn "brew install poppler failed, install it by hand for PDF rendering"
  elif command -v apt-get >/dev/null 2>&1; then
    if ! {
      as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq &&
        as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y poppler-utils
    }; then
      warn "apt-get install poppler-utils failed, install it by hand for PDF rendering"
    fi
  elif command -v dnf >/dev/null 2>&1; then
    as_root dnf install -y poppler-utils || warn "dnf install poppler-utils failed"
  elif command -v pacman >/dev/null 2>&1; then
    as_root pacman -S --noconfirm poppler || warn "pacman -S poppler failed"
  elif command -v apk >/dev/null 2>&1; then
    as_root apk add poppler-utils || warn "apk add poppler-utils failed"
  else
    warn "No package manager detected. Install poppler by hand for PDF rendering:"
    warn "  macOS: brew install poppler   Debian/Ubuntu: apt install poppler-utils"
  fi
}

if [ "$WITH_DEPS" -eq 1 ]; then
  install_poppler
else
  info "Skipped poppler (--no-deps), so PDF page rendering needs it installed by hand"
fi

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    # An install that leaves no callable command is not finished. The whole
    # point of a one-line installer is that the user never has to hunt for the
    # path, so write the entry into the shell's startup file, once, guarded by
    # a check so a second run cannot append it twice.
    if [ "$WITH_PATH_ENTRY" -eq 0 ]; then
      warn "${INSTALL_DIR} is not on PATH and --no-path was given. Add it yourself, or call the binary by full path."
    else
      case "$(basename "${SHELL:-sh}")" in
        zsh) RC="$HOME/.zshrc" ;;
        bash)
          if [ -f "$HOME/.bashrc" ]; then RC="$HOME/.bashrc"; else RC="$HOME/.bash_profile"; fi
          ;;
        *) RC="$HOME/.profile" ;;
      esac
      PATH_LINE="export PATH=\"${INSTALL_DIR}:\$PATH\""
      if [ -f "$RC" ] && grep -Fq "$PATH_LINE" "$RC"; then
        info "${RC} already puts ${INSTALL_DIR} on PATH"
      else
        printf '\n# Added by the OpenCrabs installer\n%s\n' "$PATH_LINE" >>"$RC" ||
          error "Could not write to ${RC}. Add this line to it yourself:
   ${PATH_LINE}"
        info "Added ${INSTALL_DIR} to PATH in ${RC} (open a new terminal to pick it up)"
      fi
      PATH="${INSTALL_DIR}:${PATH}"
      export PATH
    fi
    ;;
esac

# Report success only when the command a user will type actually resolves.
# A caller that asked for --no-path manages PATH itself, so a missing entry
# there is the requested outcome, not a failure.
if [ "$WITH_PATH_ENTRY" -eq 1 ]; then
  command -v opencrabs >/dev/null 2>&1 ||
    error "opencrabs was installed but is not callable.
   Add this line to your shell startup file and open a new terminal:
     export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

info "Run 'opencrabs' to get started!"
