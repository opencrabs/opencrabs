#!/usr/bin/env bash
# Prove the desktop client opens a window on a Linux machine with no GPU.
#
# Before v0.6.0 no job ever executed opencrabs-gui. Every target compiled the
# binary, stripped it and archived it, so the first time the desktop client
# ran on Linux was on a user's machine. The archive ships it now, which makes
# "it compiles" the only thing known about the file people double-click.
#
# Two things make the boot path unrunnable in an ordinary CI job:
#   - there is no display, so gpui's X11 backend has nothing to open the
#     window on. Xvfb is a real X server with no hardware behind it.
#   - there is no GPU, so Vulkan has no device to render with. mesa's
#     lavapipe (mesa-vulkan-drivers) is a software ICD, which is enough for
#     the window to be created and painted.
#
# The assertion is "a window exists", never "--version exits 0". #2841 means
# --version boots the whole application, so its exit status says nothing
# either way. This reads the X server's own window tree instead, so it only
# passes when a window was actually mapped and titled.
#
# Usage:
#   scripts/check-gui-boot.sh <path-to-opencrabs-gui> [args...]
#
# Exit 0: a window appeared.
# Exit 1: it did not, so the client is broken on Linux.
# Exit 2: the check could not run (no binary, no xvfb, no xwininfo).
#         Never report 2 as a pass.

set -uo pipefail

prog="check-gui-boot"
binary="${1:-}"
title="OpenCrabs"
timeout_secs="${OPENCRABS_GUI_BOOT_TIMEOUT:-90}"

if [ -z "$binary" ]; then
  echo "$prog: usage: $0 <path-to-opencrabs-gui> [args...]" >&2
  exit 2
fi
shift

if [ ! -x "$binary" ]; then
  echo "$prog: not an executable file: $binary" >&2
  exit 2
fi

# `xvfb-run -a` picks a free display number, so this works on a runner and on
# a developer's desktop without either one guessing a display that is taken.
# The re-exec keeps the display alive for exactly as long as this script runs.
if [ -z "${OPENCRABS_GUI_ON_XVFB:-}" ]; then
  if ! command -v xvfb-run >/dev/null 2>&1; then
    echo "$prog: xvfb-run is missing. Install the xvfb package." >&2
    exit 2
  fi
  export OPENCRABS_GUI_ON_XVFB=1
  exec xvfb-run -a -s "-screen 0 1280x800x24" "$0" "$binary" "$@"
fi

if ! command -v xwininfo >/dev/null 2>&1; then
  echo "$prog: xwininfo is missing. Install the x11-utils package." >&2
  exit 2
fi

# A throwaway home: the first run writes config, keys and a log, and a check
# must not touch the real profile of whoever runs this locally. It also makes
# the log path below knowable instead of "$HOME/.opencrabs".
workdir="$(mktemp -d)"
home="$workdir/home"
mkdir -p "$home"
gui_log="$home/.opencrabs/logs/opencrabs-gui.log"

pid=""
cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT

# #2842: an idle window under llvmpipe keeps a core busy, so the client is
# killed as soon as the window exists rather than left running for the job.
HOME="$home" "$binary" "$@" >"$workdir/stdout" 2>"$workdir/stderr" &
pid=$!

found=""
exit_status=""
deadline=$((SECONDS + timeout_secs))
while [ "$SECONDS" -lt "$deadline" ]; do
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid" 2>/dev/null
    exit_status=$?
    break
  fi
  found="$(xwininfo -root -tree 2>/dev/null | grep "\"$title\"" | head -1)"
  if [ -n "$found" ]; then
    break
  fi
  sleep 1
done

if [ -z "$found" ]; then
  if [ -n "$exit_status" ]; then
    echo "$prog: the client exited (status $exit_status) before it opened a window" >&2
  else
    echo "$prog: no \"$title\" window appeared within ${timeout_secs}s" >&2
  fi
  echo "--- window tree ---" >&2
  xwininfo -root -tree 2>/dev/null | head -40 >&2
  echo "--- gui log ($gui_log) ---" >&2
  tail -40 "$gui_log" 2>/dev/null >&2 || echo "(no log written)" >&2
  echo "--- stdout ---" >&2
  head -40 "$workdir/stdout" >&2
  echo "--- stderr ---" >&2
  head -40 "$workdir/stderr" >&2
  exit 1
fi

echo "$prog: window mapped: $found"
exit 0
