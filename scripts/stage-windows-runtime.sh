#!/usr/bin/env bash
# Copy the MSVC runtime DLLs next to a Windows build so the archive starts on a
# machine that has never had Visual Studio or the vc_redist installed (#2257).
#
# Why app-local deployment instead of a static CRT: llama.cpp's own CMakeLists
# ignores CMAKE_MSVC_RUNTIME_LIBRARY, so `+crt-static` leaves the sys crate's
# objects on /MD while Rust goes /MT, and the link dies with 16 unresolved
# `__imp_*` externals. rtk.exe is worse: it is a third-party prebuilt that
# imports VCRUNTIME140.dll and cannot be rebuilt here at all. Shipping the four
# DLLs beside both exes fixes both without waiting on anybody upstream, and
# Microsoft licenses exactly this for the VC runtime.
#
# The DLLs are read out of the Visual Studio installation on the runner. The
# gate in scripts/check-pe-imports.py then proves CLOSURE on the finished
# archive: every non-system import must be beside the exe that asks for it. If
# a future build starts importing a fifth runtime DLL, that gate names it and
# the WANTED list below grows; the staging step is not the enforcement.
#
# Usage
#   scripts/stage-windows-runtime.sh <dest-dir>
#   scripts/stage-windows-runtime.sh --self-test
#
# Exit code is 0 only when every wanted DLL was found and copied.

set -euo pipefail

# The runtime family v0.5.15 needed and did not ship. vcomp140.dll comes from
# OpenMP, which llama-cpp-sys-2 can only link statically on GNU targets
# (build.rs gates `static-openmp` on the triple containing "gnu"), so on MSVC
# it stays dynamic and travels with us.
WANTED=(msvcp140.dll vcruntime140.dll vcruntime140_1.dll vcomp140.dll)

die() {
  echo "::error::$*" >&2
  exit 1
}

# Same message, but returns instead of exiting, so a caller can assert the
# failure (the self-test does exactly that for a redist missing OpenMP).
fail() {
  echo "::error::$*" >&2
  return 1
}

# vswhere prints a Windows path ("C:\Program Files\..."), which bash cannot
# open. cygpath ships with the Git for Windows bash that Actions runs.
to_shell_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s' "$1"
  fi
}

# True when a generation directory actually carries the x64 CRT.
has_crt_dir() {
  local dir="$1" crt
  [ -d "$dir/x64" ] || return 1
  for crt in "$dir"/x64/Microsoft.VC*.CRT; do
    [ -f "$crt/msvcp140.dll" ] && return 0
  done
  return 1
}

# Highest generation wins, but only among directories that hold the CRT. A VS
# 2026 install carries both real generations (14.51.36231, VC145) and an alias
# directory named after the toolset (v145) that holds MergeModules and no x64
# folder at all. `sort -V | tail -1` prefers the alias, because "v" sorts after
# every digit, and staging then fails with four missing DLLs while looking in a
# directory that never held them. Numeric-only candidates, newest first, first
# one with a CRT wins.
newest_version_dir() {
  local root="$1"
  local candidates dir
  candidates=$(find "$root" -mindepth 1 -maxdepth 1 -type d -name '[0-9]*' -print 2>/dev/null |
    sort -V -r)
  while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    if has_crt_dir "$dir"; then
      printf '%s' "$dir"
      return 0
    fi
  done <<<"$candidates"
  fail "no generation under $root holds an x64 CRT (saw: $(ls -1 "$root" 2>/dev/null | tr '\n' ' '))"
  return 1
}

locate_redist_root() {
  if [ -n "${REDIST_ROOT_OVERRIDE:-}" ]; then
    printf '%s' "$REDIST_ROOT_OVERRIDE"
    return
  fi
  if [ -n "${VCToolsRedistDir:-}" ]; then
    to_shell_path "${VCToolsRedistDir%/}"
    return
  fi
  local vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
  [ -x "$vswhere" ] || die "vswhere.exe not found at $vswhere; cannot locate the VC redist"
  local installed
  installed=$("$vswhere" -latest -products '*' -property installationPath) ||
    die "vswhere failed"
  [ -n "$installed" ] || die "vswhere reported no Visual Studio installation"
  local root
  root="$(to_shell_path "$installed")/VC/Redist/MSVC"
  [ -d "$root" ] || die "no VC/Redist/MSVC under $installed"
  printf '%s' "$root"
}

# The CRT and OpenMP folders are named per toolset (Microsoft.VC143.CRT and
# friends), and the number changes with the VS version, so glob instead of
# pinning VC143.
copy_from_subdir() {
  local version_dir="$1" dest="$2" pattern="$3" dll="$4"
  local matches dir found=""
  matches=$(find "$version_dir/x64" -mindepth 1 -maxdepth 1 -type d -name "$pattern" 2>/dev/null || true)
  [ -n "$matches" ] || return 1
  while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    if [ -f "$dir/$dll" ]; then
      cp -f "$dir/$dll" "$dest/$dll"
      found="$dir/$dll"
      break
    fi
  done <<<"$matches"
  [ -n "$found" ] || return 1
  printf '%s' "$found"
}

stage() {
  local dest="$1"
  local root version_dir
  root=$(locate_redist_root) || return 1
  if [ ! -d "$root" ]; then
    fail "redist root does not exist: $root"
    return 1
  fi
  version_dir=$(newest_version_dir "$root") || return 1
  mkdir -p "$dest"

  local missing=() dll source
  echo "Staging the MSVC runtime into $dest"
  echo "  redist: $version_dir"
  for dll in "${WANTED[@]}"; do
    case "$dll" in
      vcomp140.dll) source=$(copy_from_subdir "$version_dir" "$dest" 'Microsoft.VC*.OpenMP' "$dll") || source="" ;;
      *) source=$(copy_from_subdir "$version_dir" "$dest" 'Microsoft.VC*.CRT' "$dll") || source="" ;;
    esac
    if [ -z "$source" ]; then
      missing+=("$dll")
      echo "  MISSING $dll"
      continue
    fi
    printf '  %-20s %8s bytes  %s\n' "$dll" "$(wc -c <"$dest/$dll" | tr -d ' ')" "$source"
  done

  if [ ${#missing[@]} -gt 0 ]; then
    fail "could not find ${missing[*]} under $version_dir/x64"
    return 1
  fi
  echo "Staged ${#WANTED[@]} runtime DLLs."
}

self_test() {
  local tmp
  tmp=$(mktemp -d)
  # Two generations, deliberately out of order, plus a decoy newer-looking
  # friend: sort -V must pick 14.44 over 14.9 and 14.29.
  local old="$tmp/14.29.30153" new="$tmp/14.44.35207"
  # The two shapes that must never win over a real generation: the toolset
  # alias (v145) sorts above every digit, and a newer generation can
  # legitimately ship without the CRT.
  mkdir -p "$tmp/v145/MergeModules" "$tmp/14.99.00000/MergeModules"
  mkdir -p "$old/x64/Microsoft.VC143.CRT" "$old/x64/Microsoft.VC143.OpenMP"
  mkdir -p "$new/x64/Microsoft.VC143.CRT" "$new/x64/Microsoft.VC143.OpenMP"
  printf 'old' >"$old/x64/Microsoft.VC143.CRT/vcruntime140.dll"
  printf 'old' >"$old/x64/Microsoft.VC143.OpenMP/vcomp140.dll"
  local dll
  for dll in msvcp140.dll vcruntime140.dll vcruntime140_1.dll; do
    printf 'new' >"$new/x64/Microsoft.VC143.CRT/$dll"
  done
  printf 'new' >"$new/x64/Microsoft.VC143.OpenMP/vcomp140.dll"

  local failures=0 dest="$tmp/dest"
  REDIST_ROOT_OVERRIDE="$tmp" stage "$dest" >/dev/null || failures=$((failures + 1))
  for dll in "${WANTED[@]}"; do
    if [ ! -f "$dest/$dll" ]; then
      echo "FAIL self-test: $dll was not staged" >&2
      failures=$((failures + 1))
      continue
    fi
    if [ "$(cat "$dest/$dll")" != "new" ]; then
      echo "FAIL self-test: $dll came from the older redist generation" >&2
      failures=$((failures + 1))
    fi
  done

  # A redist with no OpenMP folder must fail loudly, not ship an archive that
  # dies with "VCOMP140.DLL was not found" exactly as 0.5.15 did.
  local incomplete="$tmp/incomplete"
  mkdir -p "$incomplete/x64/Microsoft.VC143.CRT"
  for dll in msvcp140.dll vcruntime140.dll vcruntime140_1.dll; do
    printf 'x' >"$incomplete/x64/Microsoft.VC143.CRT/$dll"
  done
  if REDIST_ROOT_OVERRIDE="$incomplete" stage "$tmp/dest2" >/dev/null 2>&1; then
    echo "FAIL self-test: a redist without vcomp140.dll was accepted" >&2
    failures=$((failures + 1))
  fi

  rm -rf "$tmp"
  if [ "$failures" -ne 0 ]; then
    echo "self-test: FAILED ($failures)" >&2
    return 1
  fi
  echo "self-test: all checks passed"
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit 0
fi

[ $# -eq 1 ] || {
  echo "usage: $0 <dest-dir> | --self-test" >&2
  exit 2
}
stage "$1"