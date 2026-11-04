#!/usr/bin/env bash
# Fail when a Linux binary needs a newer glibc than the one we promise to ship for.
#
# A dynamically linked binary records the libc symbol version it was linked
# against, so the highest GLIBC_* tag in its dynamic symbol table is the oldest
# libc it can start on. Building on a newer image silently raises that floor:
# v0.5.13's amd64 asset came out requiring GLIBC_2.43 because the release job
# was on the moving ubuntu-latest label, which had rolled to Ubuntu 26.04
# between two tags (#2206). It verified, it downloaded, and then it refused to
# start on Ubuntu 24.04 with "version `GLIBC_2.43' not found".
#
# Running the binary on the build runner does not catch that: the build image
# always satisfies the requirement its own toolchain baked in. Comparing the
# declared floor against the tags actually in the binary does.
#
# Usage:
#   scripts/check-glibc-floor.sh <binary> <max-glibc-version>
#   scripts/check-glibc-floor.sh opencrabs 2.39
#
# Exit 0: the binary starts on the declared floor or anything older.
# Exit 1: it needs a newer glibc, so users on the oldest supported
#         distribution would get "not found" instead of a working program.
# Exit 2: the check could not run (bad arguments, missing objdump, not a
#         dynamically linked ELF). Never report 2 as a pass.

set -euo pipefail

binary="${1:-}"
floor="${2:-}"

if [ -z "$binary" ] || [ -z "$floor" ]; then
  echo "usage: $0 <binary> <max-glibc-version>   e.g. $0 opencrabs 2.39" >&2
  exit 2
fi

if [ ! -f "$binary" ]; then
  echo "check-glibc-floor: no such file: $binary" >&2
  exit 2
fi

if ! command -v objdump >/dev/null 2>&1; then
  echo "check-glibc-floor: objdump not found; binutils is required for this check" >&2
  exit 2
fi

# -T reads the dynamic symbol table, which survives strip, so this works on the
# exact artifact that gets published rather than on an unstripped build output.
#
# `|| true` is deliberate: objdump exits non-zero on anything that is not an
# ELF file and grep exits 1 when nothing matches, and either of those would
# abort this script under errexit/pipefail with status 1, which reads as
# "the floor was exceeded". A check that could not run must report 2.
required="$(
  objdump -T "$binary" 2>/dev/null \
    | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' \
    | sed 's/^GLIBC_//' \
    | sort -uV \
    | tail -1 || true
)"

if [ -z "$required" ]; then
  echo "check-glibc-floor: no GLIBC version tags in $binary" >&2
  echo "  Expected a dynamically linked ELF binary. A static or musl build has no" >&2
  echo "  glibc floor to check, so this gate does not apply to it." >&2
  exit 2
fi

# Compare major.minor only. Patch components do exist (GLIBC_2.2.5, 2.3.2) and
# they are always below a two-component floor, so ignoring them is safe and
# keeps the comparison readable.
verdict="$(
  awk -v have="$required" -v want="$floor" 'BEGIN {
    n = split(have, h, ".")
    m = split(want, w, ".")
    for (i = 1; i <= 2; i++) {
      hv = (i <= n) ? h[i] + 0 : 0
      wv = (i <= m) ? w[i] + 0 : 0
      if (hv > wv) { print "newer"; exit }
      if (hv < wv) { print "older"; exit }
    }
    print "equal"
  }'
)"

if [ "$verdict" = "newer" ]; then
  echo "check-glibc-floor: FAIL"
  echo "  binary:   $binary"
  echo "  requires: GLIBC_$required"
  echo "  floor:    GLIBC_$floor"
  echo ""
  echo "  This binary refuses to start on any distribution older than glibc $required."
  echo "  Build it on an image that ships glibc $floor or older, or raise the floor"
  echo "  deliberately in the workflow. Do not publish it as is."
  exit 1
fi

echo "check-glibc-floor: OK  $binary requires GLIBC_$required, floor is GLIBC_$floor"
