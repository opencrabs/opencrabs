#!/usr/bin/env bash
# ci-resource-sampler.sh: print the machine's resource trajectory to stdout
# while a long CI step runs, so the evidence is already in the job log by the
# time the runner kills the job (#2598).
#
# Test (Linux) dies 5 to 8 minutes into `cargo test`, every time, with
# "test-stall-watchdog: received SIGTERM from outside" followed by "The runner
# has received a shutdown signal" (runs 42086344494, 42090046034, 42099028716,
# 42103003221; zero test output in any of them). The watchdog cannot explain
# it: the signal arrives from the platform, and a killed job runs no cleanup
# step, so a file written for `actions/upload-artifact` is lost with the
# runner. A line already printed to stdout is published when it is written.
#
# Every line is one sample, greppable with `ci-resource `:
#   mem_used / mem_avail  from /proc/meminfo, the whole machine
#   swap_used             swap growth is the first sign of memory pressure
#   disk                  free space on the workspace filesystem
#   cg_current / cg_peak  this job's cgroup usage, the memory it may not exceed
#   oom_kill              cgroup oom_kill counter, nonzero after a kill
#   top                   the three largest RSS processes, so a link that ate
#                         the machine is named in the log rather than suspected
#
# Linux only by design: it reads /proc and the cgroup memory controller. On any
# other platform it prints one line and exits 0, so a caller needs no OS guard.
#
# Usage: ci-resource-sampler.sh [interval_secs]
set -u

interval="${1:-10}"
case "$interval" in
  ''|*[!0-9]*) echo "ci-resource: invalid interval '$interval', using 10s"; interval=10 ;;
esac

if [ "$(uname -s)" != "Linux" ]; then
  echo "ci-resource: not Linux ($(uname -s)); sampler idle"
  exit 0
fi

# cgroup v2 exposes the counters at the root, v1 nests them under memory/.
cgroup_dir=""
for candidate in /sys/fs/cgroup /sys/fs/cgroup/memory; do
  if [ -r "$candidate/memory.current" ] || [ -r "$candidate/memory.usage_in_bytes" ]; then
    cgroup_dir="$candidate"
    break
  fi
done

# /home/runner exists only on GitHub-hosted runners. Anywhere else fall back
# to the workspace, then to the root filesystem: an empty field would read as
# "no problem" when it is really "not measured".
disk_target=/home/runner
[ -d "$disk_target" ] || disk_target="${GITHUB_WORKSPACE:-$PWD}"
[ -d "$disk_target" ] || disk_target=/

# A missing counter prints "-", never "0": zero means "the kernel says none",
# and the two must stay distinguishable when reading the log after the fact.
read_counter() {
  if [ -n "$cgroup_dir" ] && [ -r "$cgroup_dir/$1" ]; then
    cat "$cgroup_dir/$1" 2>/dev/null
  else
    echo "-"
  fi
}

# cgroup v1 keeps the kill count inside memory.oom_control instead.
oom_kills() {
  if [ -n "$cgroup_dir" ] && [ -r "$cgroup_dir/memory.events" ]; then
    awk '/^oom_kill /{print $2; found=1} END{if (!found) print "-"}' \
      "$cgroup_dir/memory.events" 2>/dev/null
  else
    echo "-"
  fi
}

# Largest offenders first: the compile is the suspect, so a killed job shows
# which process had grown when the signal arrived.
top_rss() {
  ps -eo rss=,comm= --sort=-rss 2>/dev/null | head -3 |
    awk '{printf "%s:%dMB ", $2, $1/1024}'
}

echo "ci-resource: sampler start interval=${interval}s $(uname -srm) nproc=$(nproc 2>/dev/null || echo '?')"
echo "ci-resource: cgroup=${cgroup_dir:-none} limit=$(read_counter memory.max)"

while :; do
  stamp="$(date -u +%H:%M:%SZ)"
  mem_total="$(awk '/^MemTotal:/{printf "%d", $2/1024}' /proc/meminfo)"
  mem_avail="$(awk '/^MemAvailable:/{printf "%d", $2/1024}' /proc/meminfo)"
  swap_used="$(awk '/^SwapTotal:/{total=$2} /^SwapFree:/{free=$2} END{printf "%d", (total-free)/1024}' /proc/meminfo)"
  disk="$(df -m "$disk_target" 2>/dev/null | awk 'NR==2{printf "%sMBfree:%s", $4, $5}')"
  [ -n "$disk" ] || disk="?"
  load="$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || printf '?')"
  echo "ci-resource ${stamp} mem_used=$((mem_total - mem_avail))MB mem_avail=${mem_avail}MB swap_used=${swap_used}MB disk=${disk} load=${load} cg_current=$(read_counter memory.current) cg_peak=$(read_counter memory.peak) oom_kill=$(oom_kills) top=$(top_rss)"
  sleep "$interval"
done