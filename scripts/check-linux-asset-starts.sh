#!/usr/bin/env bash
# Run the binary out of a published Linux archive inside a container of the
# oldest distribution we support.
#
# scripts/check-glibc-floor.sh reads the GLIBC symbol versions the binary
# demands. That is necessary and it is what named the reason for #2206, but it
# only proves what the binary asks for. It never proves the binary starts.
# The v0.5.13 amd64 asset passed every check in the workflow and then refused to
# start on Ubuntu 24.04 with "version `GLIBC_2.43' not found".
#
# Running it on the build runner answers nothing: the build image always
# satisfies the floor its own toolchain baked in, which the floor script's own
# header says. The only machine that can answer "does this start for a user" is
# one running the oldest distribution we promise to support.
#
# So this extracts the binary from the exact archive that is about to be
# uploaded and runs it there. The container gets the runtime packages the README
# documents for Linux and nothing else, so it also catches a dependency that
# crept in without being written down.
#
# Usage:
#   scripts/check-linux-asset-starts.sh <archive.tar.gz> <max-glibc-version>
#   scripts/check-linux-asset-starts.sh opencrabs-v0.5.14-linux-amd64.tar.gz 2.39
#
# Exit 0: the published binary starts on the oldest supported distribution.
# Exit 1: it does not start there, so users on that distribution get a crash
#         or a loader error instead of a working program.
# Exit 2: the check could not run (bad arguments, no docker, unknown floor).
#         Never report 2 as a pass.

set -euo pipefail

archive="${1:-}"
floor="${2:-}"

if [ -z "$archive" ] || [ -z "$floor" ]; then
  echo "usage: $0 <archive.tar.gz> <max-glibc-version>   e.g. $0 opencrabs-v0.5.14-linux-amd64.tar.gz 2.39" >&2
  exit 2
fi

if [ ! -f "$archive" ]; then
  echo "check-linux-asset-starts: no such file: $archive" >&2
  exit 2
fi

# The image is keyed off the declared floor rather than hardcoded at the call
# site, so the tested distribution and the promised floor cannot drift apart.
# A new floor has to name its image here, deliberately.
case "$floor" in
  2.39) image="ubuntu:24.04" ;;
  *)
    echo "check-linux-asset-starts: no container image is mapped to glibc $floor" >&2
    echo "  Add it to the case statement in $0 and in the workflow's GLIBC_FLOOR." >&2
    exit 2
    ;;
esac

# Runtime packages for a Linux install. These are the shared objects the binary
# actually links against, minus the C library every image already carries:
#   libgomp.so.1      -> libgomp1     (OpenMP, the local embedding engine)
#   libstdc++.so.6    -> libstdc++6
#   libssl.so.3       -> libssl3t64   (on 24.04; the package is libssl3t64, and
#                                      there is no libssl3 to install)
# libasound is deliberately absent: it is not in the binary's NEEDED list, it is
# opened at runtime for local speech-to-text, so its absence does not stop the
# program from starting. Listing it here would have made this check fail on a
# distribution where the package name differs, which is a false alarm about the
# asset rather than a finding about it.
runtime_packages="libgomp1 libstdc++6 libssl3t64"

# Fail closed. A skipped container test would leave the upload unguarded, which
# is the state this script exists to end.
if ! command -v docker >/dev/null 2>&1; then
  echo "check-linux-asset-starts: docker not found; this check requires docker and cannot be skipped" >&2
  exit 2
fi

if ! docker info >/dev/null 2>&1; then
  echo "check-linux-asset-starts: docker is installed but not reachable (daemon down?)" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Extract only the binary: the archive also carries rtk, the .desktop entry and
# the icon, and none of those are what a user starts.
if ! tar xzf "$archive" -C "$work" opencrabs 2>/dev/null; then
  echo "check-linux-asset-starts: $archive has no opencrabs member" >&2
  tar tzf "$archive" >&2 || true
  exit 2
fi

chmod +x "$work/opencrabs"

echo "check-linux-asset-starts: running $archive in $image (glibc floor $floor)"

# The binary is streamed into the container rather than bind-mounted: a mount
# depends on the daemon sharing that path, which is true on a Linux runner and
# not always true of a Docker Desktop or colima setup. Stdin has no such
# requirement, so this behaves the same everywhere.
#
# The loader error is the whole point, so its output goes through unedited.
if ! tar czf - -C "$work" opencrabs | docker run --rm -i "$image" bash -c "
  mkdir -p /asset && tar xzf - -C /asset &&
  apt-get update -qq &&
  apt-get install -y -qq $runtime_packages &&
  /asset/opencrabs --version
"; then
  echo "" >&2
  echo "check-linux-asset-starts: FAIL" >&2
  echo "  archive: $archive" >&2
  echo "  image:   $image" >&2
  echo "" >&2
  echo "  The published binary does not start on the oldest distribution we support." >&2
  echo "  Do not publish this asset. Build it on an image that ships glibc $floor or" >&2
  echo "  older, and check that the runtime packages in the README are complete." >&2
  exit 1
fi

echo "check-linux-asset-starts: OK  $archive starts on $image"
