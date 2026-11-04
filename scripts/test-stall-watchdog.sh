#!/usr/bin/env bash
# test-stall-watchdog.sh: run a command (the test suite), and if it freezes,
# say so with thread stacks instead of silence (#1936).
#
# The gate has burned its whole timeout with every test-thread slot blocked
# and no verdict: the runner kills the job, so the blocked stacks are never
# dumped. This wrapper watches the command's own log for completed-test
# lines. Once tests are reporting results, a gap of WATCHDOG_STALL_SECS with
# no new result line is declared a stall: every descendant process gets a
# thread dump, its subtree is killed, and the script exits 97 with a
# "STALL DETECTED" marker, so the failure surfaces minutes after it starts
# instead of at the runner's wall.
#
# Before the first result line the stall clock does not run: the same
# command legitimately spends 20+ cold-cache minutes in "Compiling", and
# killing a build would misdiagnose a lock bug as a timeout.
#
# Test seams (unset in CI; src/tests/test_stall_watchdog_test.rs uses them):
#   WATCHDOG_STALL_SECS - stall threshold, seconds (default 240)
#   WATCHDOG_POLL_SECS  - poll interval, fractions allowed (default 2)
#   WATCHDOG_DUMP_ON    - file appended per dump (empty disables)
#   WATCHDOG_DUMPER     - command prefix used to dump a pid, called as
#                         "$WATCHDOG_DUMPER <pid>"; default is per-OS
#                         (lldb on macOS, gdb on Linux, none elsewhere)
set -u

STALL_SECS="${WATCHDOG_STALL_SECS:-300}"
POLL_SECS="${WATCHDOG_POLL_SECS:-2}"
DUMP_ON="${WATCHDOG_DUMP_ON:-}"
DUMPER="${WATCHDOG_DUMPER:-}"

dump_pid() {
  local pid="$1"
  if [ -n "$DUMPER" ]; then
    "$DUMPER" "$pid"
    return
  fi
  case "$(uname -s)" in
    # lldb "thread backtrace all" dumps every thread of the target: the
    # blocked-slot picture the issue asks for.
    Darwin) lldb -p "$pid" -b -o "thread backtrace all" ;;
    Linux)
      # Hosted runners allow the ptrace attach. Best-effort sysctl: a
      # self-hosted machine without sudo just fails the dump loudly
      # instead of hanging anything.
      sudo -n sysctl -w kernel.yama.ptrace_scope=0 2>/dev/null || true
      gdb -p "$pid" -batch -ex "thread apply all bt" ;;
    *)
      # Windows has no cheap in-band stack dumper; the watchdog still
      # fails fast with the marker and a process list. Stacks stay the
      # unix half of #1936, per the issue's own ubuntu evidence.
      ps -ef 2>/dev/null || true ;;
  esac
}

# All descendants of a pid, depth-first. The stalled test binary is a
# child of cargo, so one pgrep level is not enough.
collect_tree() {
  local top="$1" kid
  for kid in $(pgrep -P "$top" 2>/dev/null || true); do
    echo "$kid"
    collect_tree "$kid"
  done
}

kill_tree() {
  local top="$1" kid kids
  kids="$(pgrep -P "$top" 2>/dev/null || true)"
  for kid in $kids; do kill_tree "$kid"; done
  kill -TERM "$top" 2>/dev/null || true
}

log="$(mktemp "${TMPDIR:-/tmp}/stall-watchdog.XXXXXX")"
trap 'rm -f "$log"' EXIT

# A killed run must not take its own evidence with it. When a job is
# reclaimed or cancelled, the runner sends SIGTERM to the step; bash then
# fires the EXIT trap above, deletes the buffer, and dies from the signal
# before the `cat "$log"` at the bottom can print anything. The job log is
# left holding the start line, an empty middle, and exit 143, so the killer
# can never be identified from the evidence (2026-10-25 9m19s, 2026-10-27
# 8m08s, 2026-10-28 7m37s, all on Test (Linux), zero test output each time).
# Dump whatever the command produced so far, then take its subtree with us
# instead of leaving an orphaned compile behind.
on_signal() {
  local sig="$1" code="$2"
  echo "::error::test-stall-watchdog: received SIG${sig} from outside; dumping the buffered command output before exit."
  echo "test-stall-watchdog: SIG${sig} received for '${cmd_pid:-no command yet}', partial output follows."
  if [ -s "$log" ]; then
    echo "----- buffered output so far (partial, SIG${sig}) -----"
    cat "$log"
    echo "----- end buffered output (partial, SIG${sig}) -----"
  else
    echo "----- buffer was empty when SIG${sig} arrived -----"
  fi
  if [ -n "${cmd_pid:-}" ]; then
    kill_tree "$cmd_pid"
  fi
  exit "$code"
}
trap 'on_signal TERM 143' TERM
trap 'on_signal INT 130' INT

count_lines() { wc -l < "$log" | tr -d '[:space:]'; }
# A result line is a completed test; libtest prints one per test even with
# output captured. It is the only progress signal that proves the threadpool
# is draining.
has_result_line() { grep -q -E '\.\.\. (ok|FAILED|ignored)$' "$log" 2>/dev/null; }

echo "+ $* (watchdog: stall after ${STALL_SECS}s of result silence)"
"$@" >"$log" 2>&1 &
cmd_pid=$!

stalled=0
last_lines=0
last_progress="$(date +%s)"
while kill -0 "$cmd_pid" 2>/dev/null; do
  sleep "$POLL_SECS"
  if has_result_line; then
    now="$(date +%s)"
    lines="$(count_lines)"
    if [ "$lines" -eq "$last_lines" ]; then
      if [ $(( now - last_progress )) -ge "$STALL_SECS" ]; then
        stalled=1
        break
      fi
    else
      last_lines="$lines"
      last_progress="$now"
    fi
  fi
done

rc=0
if [ "$stalled" -eq 1 ]; then
  echo "::error::STALL DETECTED (#1936): no completed-test line for ${STALL_SECS}s while '${cmd_pid}' was alive. Dumping thread stacks of its descendants."
  echo "STALL DETECTED (#1936): no completed-test line for ${STALL_SECS}s; dumping descendant stacks."
  # Dump first: a dead process has no stacks. The command is only
  # killed after every descendant has been inspected.
  for pid in "$cmd_pid" $(collect_tree "$cmd_pid"); do
    echo "--- stack dump of pid $pid ---"
    [ -n "$DUMP_ON" ] && echo "dumped:$pid" >> "$DUMP_ON"
    dump_pid "$pid" || echo "(dump failed for pid $pid)"
  done
  kill_tree "$cmd_pid"
fi
wait "$cmd_pid" 2>/dev/null || rc=$?

cat "$log"

if [ "$stalled" -eq 1 ]; then
  echo "test-stall-watchdog: command $cmd_pid was stalled; killed after dumping stacks (exit 97, #1936)"
  exit 97
fi
exit "$rc"
