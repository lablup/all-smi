#!/usr/bin/env bash
#
# Copyright 2025 Lablup Inc. and Jeongkyu Shin
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# Measure all-smi local-mode CPU usage across collection intervals.
#
# Issue #288: the local-mode default interval moved from 3s to 1s on Apple
# Silicon and to 2s elsewhere (PR #286). Only Apple Silicon was benchmarked.
# This script exists so results from other platforms are produced the same way
# and are therefore comparable.
#
# USAGE
#
#   scripts/bench-local-interval.sh [-d SECONDS] [-b PATH] [-i "LIST"]
#
#     -d SECONDS   measurement window per configuration (default 60)
#     -b PATH      all-smi binary (default ./target/release/all-smi)
#     -i "LIST"    space-separated intervals to test (default "1 2 3");
#                  the no-flag default configuration is always measured
#
#   Build first:  cargo build --release --bin all-smi
#
# REQUIREMENTS
#
#   tmux, for a detached session at a fixed 200x50 size. Terminal size affects
#   render cost, so a fixed size is what makes runs on different machines
#   comparable. macOS and Linux only; on Windows, run under WSL or report that
#   the platform is untested.
#
# METHOD
#
#   CPU is derived from the process CPU-time delta over the window, not from
#   `ps -o %cpu`. That column means different things per platform: a decaying
#   recent average on macOS, a lifetime average on Linux. A CPU-time delta
#   divided by wall time is the same quantity everywhere. Values are percent of
#   one core, so a fully busy single thread reads 100%.
#
#   The first WARMUP_SECS after launch are discarded so one-time startup work
#   (reader initialisation, first collection, first render) does not land in
#   the window.
#
# REPORTING
#
#   Paste the whole output, environment block included, into issue #288. The
#   environment block is what lets a reader tell whether two results are
#   comparable.

set -euo pipefail

DURATION=60
BIN="./target/release/all-smi"
INTERVALS="1 2 3"
WARMUP_SECS=8
TMUX_COLS=200
TMUX_ROWS=50

# Print the header comment block: everything from the first line after the
# licence block down to the last comment line before the code starts. Derived
# from the file rather than duplicated, so editing the header cannot leave the
# help text stale.
usage() {
  awk '
    /^# Measure all-smi/ { show = 1 }
    show && !/^#/ { exit }
    show { sub(/^# ?/, ""); print }
  ' "$0"
}

while getopts "d:b:i:h" opt; do
  case "$opt" in
    d) DURATION="$OPTARG" ;;
    b) BIN="$OPTARG" ;;
    i) INTERVALS="$OPTARG" ;;
    h) usage; exit 0 ;;
    *) echo "run '$0 -h' for usage" >&2; exit 2 ;;
  esac
done

command -v tmux >/dev/null 2>&1 || { echo "error: tmux is required" >&2; exit 1; }
[ -x "$BIN" ] || { echo "error: no executable at $BIN (cargo build --release --bin all-smi)" >&2; exit 1; }

OS="$(uname -s)"
case "$OS" in
  Linux)
    CLK_TCK="$(getconf CLK_TCK 2>/dev/null || echo 100)"
    # utime (field 14) + stime (field 15) in clock ticks. Fields are counted
    # from after the comm field, which may itself contain spaces and
    # parentheses, so split on the last ')' rather than on whitespace.
    cpu_time_seconds() {
      local stat rest
      stat="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
      rest="${stat##*) }"
      awk -v t="$rest" -v hz="$CLK_TCK" 'BEGIN { split(t, f, " "); print (f[12] + f[13]) / hz }'
    }
    ;;
  Darwin)
    # `ps -o cputime=` prints [[DD-]HH:]MM:SS.CC on BSD ps, so centisecond
    # resolution is available without reading kernel structures.
    cpu_time_seconds() {
      local t
      t="$(ps -o cputime= -p "$1" 2>/dev/null | tr -d ' ')" || return 1
      [ -n "$t" ] || return 1
      awk -v t="$t" 'BEGIN {
        n = split(t, p, ":")
        # Optional leading DD- on the first field.
        if (n >= 1 && index(p[1], "-") > 0) { split(p[1], d, "-"); days = d[1]; p[1] = d[2] }
        s = 0
        for (i = 1; i <= n; i++) s = s * 60 + p[i]
        print s + days * 86400
      }'
    }
    ;;
  *)
    echo "error: unsupported platform '$OS' (macOS and Linux only)" >&2
    exit 1
    ;;
esac

detect_gpu() {
  if command -v nvidia-smi >/dev/null 2>&1; then
    local names
    names="$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | sort | uniq -c \
             | awk '{ $1 = $1 "x"; print }' | paste -sd'; ' -)"
    [ -n "$names" ] && { echo "$names"; return; }
  fi
  if command -v rocm-smi >/dev/null 2>&1; then
    echo "AMD (rocm-smi present)"; return
  fi
  if [ "$OS" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]; then
    # Integrated on Apple Silicon, so the SoC name is the GPU name.
    echo "$(sysctl -n machdep.cpu.brand_string 2>/dev/null) (integrated)"; return
  fi
  echo "unknown"
}

detect_cpu() {
  case "$OS" in
    Darwin) sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown ;;
    Linux)
      # `model name` is an x86 field. aarch64 /proc/cpuinfo carries
      # `CPU implementer`/`CPU part` ID pairs instead, so the awk prints an
      # empty string and still exits 0, which is why a trailing
      # `|| echo unknown` does not catch it. lscpu decodes those IDs, and on
      # a heterogeneous part prints one `Model name` per core cluster, so
      # join them: which clusters exist is itself worth reporting, because
      # the same work costs different CPU time on each.
      local name
      name="$(awk -F': ' '/model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null)"
      if [ -z "$name" ]; then
        name="$(lscpu 2>/dev/null | awk -F': +' '
          /^Model name/ { if (seen[$2]++) next; n[++c] = $2 }
          END { for (i = 1; i <= c; i++) printf "%s%s", (i > 1 ? " + " : ""), n[i] }')"
      fi
      echo "${name:-unknown}"
      ;;
  esac
}

core_count() {
  case "$OS" in
    Darwin) sysctl -n hw.ncpu 2>/dev/null || echo "?" ;;
    Linux) nproc 2>/dev/null || echo "?" ;;
  esac
}

echo "=== environment ==="
printf '  all-smi       %s\n' "$("$BIN" --version 2>/dev/null | head -1 || echo unknown)"
printf '  binary        %s\n' "$BIN"
printf '  os            %s %s (%s)\n' "$OS" "$(uname -r)" "$(uname -m)"
printf '  cpu           %s (%s cores)\n' "$(detect_cpu)" "$(core_count)"
printf '  gpu           %s\n' "$(detect_gpu)"
printf '  processes     %s\n' "$(($(ps -A 2>/dev/null | wc -l) - 1))"
printf '  terminal      %sx%s (tmux)\n' "$TMUX_COLS" "$TMUX_ROWS"
printf '  window        %ss measured after %ss warmup\n' "$DURATION" "$WARMUP_SECS"
echo

BIN_NAME="$(basename "$BIN")"

# PIDs of every already-running process named like the binary. Used to tell our
# own child apart from an all-smi the operator happens to be running, and from
# the shell tmux wraps the command in. Matching on the command line instead
# would pick up that wrapper shell, whose CPU and RSS are not what we want.
existing_pids() { pgrep -x "$BIN_NAME" 2>/dev/null | sort || true; }

# Measure one configuration. $1 is a label, the rest are extra binary args.
measure() {
  local label="$1"; shift
  local session="all_smi_bench_$$_${label//[^0-9a-zA-Z]/_}"

  local before after
  before="$(existing_pids)"

  tmux kill-session -t "$session" 2>/dev/null || true
  tmux new-session -d -s "$session" -x "$TMUX_COLS" -y "$TMUX_ROWS" \
    "$BIN local $*" 2>/dev/null

  local pid=""
  for _ in $(seq 1 20); do
    sleep 0.5
    after="$(existing_pids)"
    pid="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -1)"
    [ -n "$pid" ] && break
  done
  if [ -z "$pid" ]; then
    printf '  %-14s FAILED to start\n' "$label"
    tmux kill-session -t "$session" 2>/dev/null || true
    return
  fi

  sleep "$WARMUP_SECS"

  local t0 c0 t1 c1
  c0="$(cpu_time_seconds "$pid")" || { printf '  %-14s FAILED to read cpu time\n' "$label"; return; }
  t0="$(date +%s)"
  sleep "$DURATION"
  c1="$(cpu_time_seconds "$pid")" || { printf '  %-14s process exited early\n' "$label"; return; }
  t1="$(date +%s)"

  local rss
  rss="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || echo 0)"

  awk -v label="$label" -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" -v rss="$rss" 'BEGIN {
    wall = t1 - t0
    if (wall <= 0) { printf "  %-14s invalid window\n", label; exit }
    printf "  %-14s cpu=%6.2f%%   cpu_time=%.2fs / %ds   rss=%dMB\n",
           label, (c1 - c0) / wall * 100, c1 - c0, wall, rss / 1024
  }'

  tmux kill-session -t "$session" 2>/dev/null || true
  sleep 2
}

echo "=== results (percent of one core) ==="
measure "default"
for iv in $INTERVALS; do
  measure "-i ${iv}s" -i "$iv"
done
echo
echo "Report these numbers with the environment block above on issue #288."
