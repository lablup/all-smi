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
#                                   [-c CPUS] [-r COUNT]
#
#     -d SECONDS   measurement window per configuration (default 60)
#     -b PATH      all-smi binary (default ./target/release/all-smi)
#     -i "LIST"    space-separated intervals to test (default "1 2 3");
#                  the no-flag default configuration is always measured
#     -c CPUS      pin the run to a CPU list, e.g. "5-9,15-19" (Linux only).
#                  On a heterogeneous machine, see COMPARABILITY below
#     -r COUNT     repeats per configuration (default 1). With more than one,
#                  the mean and standard deviation are reported
#
#   Build first:  cargo build --release --bin all-smi
#                 On Linux this needs libdrm-dev: without it the link fails on
#                 -ldrm and -ldrm_amdgpu even on a host with no AMD GPU,
#                 because libamdgpu_top is a hard dependency of the glibc
#                 Linux target
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
# COMPARABILITY ACROSS MACHINES
#
#   Percent of one core is not a single quantity on a heterogeneous CPU. ARM
#   big.LITTLE parts, Intel P/E hybrids, and Apple Silicon all mix core types,
#   and identical work costs a different amount of CPU time on each type. On an
#   NVIDIA GB10 (Cortex-X925 at 3.9GHz plus Cortex-A725 at 2.8GHz), pinning the
#   same run to one cluster or the other moves the result by about 1.5x, which
#   is larger than the interval effect this script exists to measure. Unpinned
#   runs land somewhere between the two, wherever the scheduler happened to put
#   the threads, which is also why they vary more between repeats.
#
#   The durable number is therefore the ratio between two intervals measured on
#   one host, not the absolute percentage. An absolute percentage is comparable
#   to another machine's only when both state their core placement. The
#   environment block prints the detected topology and the affinity in use so
#   that context travels with the numbers.
#
#   On a heterogeneous host, prefer pinning one cluster with -c and say which
#   one you pinned. Pinning also cuts run-to-run variance substantially. Use -r
#   to average several windows when the effect you are measuring is close to
#   the spread between repeats.
#
#   Run -c on bare metal. Inside a container without a cpuset, all-smi sizes
#   its own CPU view from sched_getaffinity, so pinning would also shrink the
#   set of cores it parses and renders, changing the work being measured
#   instead of only where that work runs.
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
CPUSET=""
REPEATS=1
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

while getopts "d:b:i:c:r:h" opt; do
  case "$opt" in
    d) DURATION="$OPTARG" ;;
    b) BIN="$OPTARG" ;;
    i) INTERVALS="$OPTARG" ;;
    c) CPUSET="$OPTARG" ;;
    r) REPEATS="$OPTARG" ;;
    h) usage; exit 0 ;;
    *) echo "run '$0 -h' for usage" >&2; exit 2 ;;
  esac
done

command -v tmux >/dev/null 2>&1 || { echo "error: tmux is required" >&2; exit 1; }
[ -x "$BIN" ] || { echo "error: no executable at $BIN (cargo build --release --bin all-smi)" >&2; exit 1; }

case "$REPEATS" in
  ''|*[!0-9]*) echo "error: -r takes a positive integer, got '$REPEATS'" >&2; exit 2 ;;
esac
[ "$REPEATS" -ge 1 ] || { echo "error: -r must be at least 1" >&2; exit 2; }

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

# Prefix prepended to the launch command when -c is given. taskset execs the
# target in place, so the running process keeps the binary's name and the
# pgrep -x detection in measure_once() is unaffected; a wrapper that forked
# instead would break it.
LAUNCH_PREFIX=""
AFFINITY_DESC="unpinned"
if [ -n "$CPUSET" ]; then
  case "$OS" in
    Linux)
      command -v taskset >/dev/null 2>&1 ||
        { echo "error: -c requires taskset (util-linux)" >&2; exit 1; }
      # taskset only fails when *no* CPU in the list exists, so it rejects
      # "99-200" but silently accepts "0,99" and narrows it to CPU 0. Use it
      # to catch the wholly invalid case, then read back the mask actually in
      # force so the affinity line reports what ran, not what was asked for.
      taskset -c "$CPUSET" true >/dev/null 2>&1 ||
        { echo "error: -c '$CPUSET' matches no CPU on this machine" >&2; exit 1; }
      LAUNCH_PREFIX="taskset -c $CPUSET "
      effective="$(taskset -c "$CPUSET" sh -c 'taskset -pc $$' 2>/dev/null |
                   sed 's/.*list: *//' || true)"
      if [ -n "$effective" ] && [ "$effective" != "$CPUSET" ]; then
        AFFINITY_DESC="cpus $effective (taskset, requested $CPUSET)"
      else
        AFFINITY_DESC="cpus $CPUSET (taskset)"
      fi
      ;;
    Darwin)
      echo "error: -c is not supported on macOS. There is no way to select a specific CPU set: thread_policy_set affinity is a cache-locality hint that Apple Silicon reports as unsupported, and only E-core confinement is reachable at all, via taskpolicy -b, which also changes scheduling priority." >&2
      exit 1
      ;;
  esac
fi

# Report core types, so a reader can tell a heterogeneous host from a uniform
# one. Grouping by maximum frequency separates ARM big.LITTLE clusters and
# Intel P/E cores alike; cpu_capacity is the device-tree fallback for ARM
# systems without cpufreq. When neither is readable the topology is reported
# as unknown rather than assumed uniform, because assuming uniform is exactly
# the error this block exists to prevent.
linux_topology() {
  local src d file cpu key lines=""
  if [ -r /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq ]; then
    src=cpufreq
  elif [ -r /sys/devices/system/cpu/cpu0/cpu_capacity ]; then
    src=capacity
  else
    echo "unknown (no cpufreq or cpu_capacity)"
    return
  fi

  for d in /sys/devices/system/cpu/cpu[0-9]*; do
    cpu="${d##*/cpu}"
    case "$cpu" in ''|*[!0-9]*) continue ;; esac
    if [ "$src" = cpufreq ]; then
      file="$d/cpufreq/cpuinfo_max_freq"
    else
      file="$d/cpu_capacity"
    fi
    key="$(cat "$file" 2>/dev/null || true)"
    [ -n "$key" ] || continue
    lines="${lines}${key} ${cpu}
"
  done
  [ -n "$lines" ] || { echo "unknown (no cpufreq or cpu_capacity)"; return; }

  # Sort by key descending so awk walks from the fastest core type down and
  # can bucket by proximity in one pass.
  #
  # Cores of one type do not all report an identical key. This machine's
  # cpu_capacity reads 718, 731, 997, 1017, and 1024 across a two-cluster
  # part, and Intel parts with per-core turbo binning give favoured P-cores a
  # higher cpuinfo_max_freq than their siblings. Exact-equality grouping would
  # report five tiers here, one of them a single CPU, which is worse than
  # useless on precisely the hosts this exists for. Bucket keys within
  # TOLERANCE of the group's fastest member instead, comparing against that
  # fixed representative rather than the previous row so a long run of small
  # steps cannot drift one bucket across a real cluster boundary.
  printf '%s' "$lines" | sort -k1,1nr -k2,2n | awk -v src="$src" -v tol=0.05 '
    function ranges(s,   a, n, i, j, t, out) {
      n = split(s, a, " ")
      # A bucket can merge several keys, whose CPU runs interleave, so sort
      # numerically before collapsing rather than trusting the input order.
      for (i = 2; i <= n; i++) {
        t = a[i] + 0
        for (j = i - 1; j >= 1 && a[j] + 0 > t; j--) a[j + 1] = a[j]
        a[j + 1] = t
      }
      out = ""
      i = 1
      while (i <= n) {
        j = i
        while (j + 1 <= n && a[j + 1] + 0 == a[j] + 0 + 1) j++
        out = out (out == "" ? "" : ",") (i == j ? a[i] : a[i] "-" a[j])
        i = j + 1
      }
      return out
    }
    {
      if (g == 0 || $1 + 0 < rep * (1 - tol)) { g++; rep = $1 + 0; gkey[g] = $1 + 0 }
      gcnt[g]++
      glist[g] = glist[g] " " $2
    }
    END {
      out = ""
      for (i = 1; i <= g; i++) {
        lbl = (src == "cpufreq") ? sprintf("%.2fGHz", gkey[i] / 1000000) : sprintf("capacity %d", gkey[i])
        out = out (i > 1 ? ", " : "") gcnt[i] "x " lbl
        if (g > 1) out = out " (cpus " ranges(glist[i]) ")"
      }
      printf "%s: %s\n", (g > 1 ? "heterogeneous" : "uniform"), out
    }'
}

# macOS exposes perflevels rather than per-CPU frequencies, and gives no way
# to address individual cores, so no CPU lists are printed here.
darwin_topology() {
  local levels i name count out=""
  levels="$(sysctl -n hw.nperflevels 2>/dev/null || echo 1)"
  case "$levels" in ''|*[!0-9]*) levels=1 ;; esac
  if [ "$levels" -le 1 ]; then
    echo "uniform: $(sysctl -n hw.logicalcpu 2>/dev/null || echo '?') logical cores"
    return
  fi
  i=0
  while [ "$i" -lt "$levels" ]; do
    name="$(sysctl -n "hw.perflevel${i}.name" 2>/dev/null || echo "level${i}")"
    count="$(sysctl -n "hw.perflevel${i}.logicalcpu" 2>/dev/null || echo '?')"
    out="${out}${out:+, }${count}x ${name}"
    i=$((i + 1))
  done
  echo "heterogeneous: $out"
}

detect_topology() {
  case "$OS" in
    Linux) linux_topology ;;
    Darwin) darwin_topology ;;
  esac
}

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

TOPOLOGY="$(detect_topology)"

# On a heterogeneous host an unpinned run is a mix of core types, and the
# reader needs to know that before comparing the number to another machine's.
# Only Linux is pointed at -c: every Apple Silicon Mac reports two perflevels
# and so lands here, and telling those runs to use a flag that macOS rejects
# would be advice that never works.
case "$TOPOLOGY" in
  heterogeneous*)
    if [ -z "$CPUSET" ]; then
      case "$OS" in
        Linux) AFFINITY_DESC="unpinned (mixed core types: threads may migrate, see -c)" ;;
        Darwin) AFFINITY_DESC="unpinned (mixed core types: threads may migrate; macOS has no CPU affinity control)" ;;
      esac
    fi
    ;;
esac

echo "=== environment ==="
printf '  all-smi       %s\n' "$("$BIN" --version 2>/dev/null | head -1 || echo unknown)"
printf '  binary        %s\n' "$BIN"
printf '  os            %s %s (%s)\n' "$OS" "$(uname -r)" "$(uname -m)"
printf '  cpu           %s (%s cores)\n' "$(detect_cpu)" "$(core_count)"
printf '  topology      %s\n' "$TOPOLOGY"
printf '  affinity      %s\n' "$AFFINITY_DESC"
printf '  gpu           %s\n' "$(detect_gpu)"
printf '  processes     %s\n' "$(($(ps -A 2>/dev/null | wc -l) - 1))"
printf '  terminal      %sx%s (tmux)\n' "$TMUX_COLS" "$TMUX_ROWS"
printf '  window        %ss measured after %ss warmup%s\n' "$DURATION" "$WARMUP_SECS" \
  "$([ "$REPEATS" -gt 1 ] && printf ', %s repeats' "$REPEATS" || true)"
echo

BIN_NAME="$(basename "$BIN")"

# PIDs of every already-running process named like the binary. Used to tell our
# own child apart from an all-smi the operator happens to be running, and from
# the shell tmux wraps the command in. Matching on the command line instead
# would pick up that wrapper shell, whose CPU and RSS are not what we want.
existing_pids() { pgrep -x "$BIN_NAME" 2>/dev/null | sort || true; }

# Run one measurement window. Echoes "cpu_seconds wall_seconds rss_kb" on
# success. Failures are reported through the exit code so the caller can name
# the reason after all repeats have run: 1 could not start, 2 could not read
# CPU time, 3 exited early, 4 window was not positive.
measure_once() {
  local label="$1" rep="$2"; shift 2
  local session="all_smi_bench_$$_${label//[^0-9a-zA-Z]/_}_${rep}"

  local before after
  before="$(existing_pids)"

  tmux kill-session -t "$session" >/dev/null 2>&1 || true
  tmux new-session -d -s "$session" -x "$TMUX_COLS" -y "$TMUX_ROWS" \
    "${LAUNCH_PREFIX}$BIN local $*" >/dev/null 2>&1

  local pid=""
  for _ in $(seq 1 20); do
    sleep 0.5
    after="$(existing_pids)"
    pid="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -1)"
    [ -n "$pid" ] && break
  done
  if [ -z "$pid" ]; then
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    return 1
  fi

  sleep "$WARMUP_SECS"

  local t0 c0 t1 c1 rss
  c0="$(cpu_time_seconds "$pid")" ||
    { tmux kill-session -t "$session" >/dev/null 2>&1 || true; return 2; }
  t0="$(date +%s)"
  sleep "$DURATION"
  c1="$(cpu_time_seconds "$pid")" ||
    { tmux kill-session -t "$session" >/dev/null 2>&1 || true; return 3; }
  t1="$(date +%s)"

  rss="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
  [ -n "$rss" ] || rss=0

  tmux kill-session -t "$session" >/dev/null 2>&1 || true

  awk -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" -v rss="$rss" 'BEGIN {
    wall = t1 - t0
    if (wall <= 0) exit 1
    printf "%.6f %d %d\n", c1 - c0, wall, rss
  }' || return 4
}

# Measure one configuration REPEATS times and print a single summary line.
# $1 is a label, the rest are extra binary args.
measure() {
  local label="$1"; shift
  local samples="" failures="" sample rep=1 rc

  while [ "$rep" -le "$REPEATS" ]; do
    rc=0
    sample="$(measure_once "$label" "$rep" "$@")" || rc=$?
    if [ "$rc" -eq 0 ]; then
      samples="${samples}${sample}
"
    else
      case "$rc" in
        1) failures="${failures}${failures:+, }could not start" ;;
        2) failures="${failures}${failures:+, }could not read cpu time" ;;
        3) failures="${failures}${failures:+, }exited early" ;;
        *) failures="${failures}${failures:+, }invalid window" ;;
      esac
    fi
    rep=$((rep + 1))
    sleep 2
  done

  if [ -z "$samples" ]; then
    printf '  %-14s FAILED (%s)\n' "$label" "$failures"
    return
  fi

  # Averaging percentages per window rather than dividing summed CPU time by
  # summed wall time keeps each window weighted equally even if one ran long.
  printf '%s' "$samples" | awk -v label="$label" -v want="$REPEATS" -v reasons="$failures" '
    { pct = $1 / $2 * 100; sum += pct; sumsq += pct * pct; ct += $1; wall += $2; rss += $3; n++ }
    END {
      mean = sum / n
      if (n > 1) {
        var = (sumsq - n * mean * mean) / (n - 1)
        if (var < 0) var = 0
        printf "  %-14s cpu=%6.2f%% +/- %.2f (n=%d)   cpu_time=%.2fs / %ds   rss=%dMB",
               label, mean, sqrt(var), n, ct / n, wall / n + 0.5, rss / n / 1024
      } else {
        printf "  %-14s cpu=%6.2f%%   cpu_time=%.2fs / %ds   rss=%dMB",
               label, mean, ct, wall, rss / 1024
      }
      # Name the failures here too, not only when every window failed: a
      # partial run that silently reported a smaller n would look like a
      # deliberately shorter measurement.
      if (n < want) printf "   [%d/%d windows ok: %s]", n, want, reasons
      printf "\n"
    }'
}

echo "=== results (percent of one core) ==="
measure "default"
for iv in $INTERVALS; do
  measure "-i ${iv}s" -i "$iv"
done
echo
echo "Report these numbers with the environment block above on issue #288."
