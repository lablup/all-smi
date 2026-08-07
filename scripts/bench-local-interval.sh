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
#                                   [-c CPUS] [-r COUNT] [-h]
#
#     -d SECONDS   measurement window per configuration, whole seconds
#                  (default 60)
#     -b PATH      all-smi binary (default ./target/release/all-smi)
#     -i "LIST"    space-separated intervals to test, whole seconds
#                  (default "1 2 3"); the no-flag default configuration is
#                  always measured
#     -c CPUS      pin the run to a CPU list, e.g. "5-9,15-19" (Linux only).
#                  On a heterogeneous machine, see COMPARABILITY below
#     -r COUNT     repeats per configuration (default 1). With more than one,
#                  the mean and standard deviation are reported
#     -h           print this text and exit
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
#   An affinity mask does not have to come from -c. Running under taskset, in a
#   container with a cpuset, under a systemd unit with AllowedCPUs=, or under a
#   batch scheduler that pins jobs all narrow the mask before this script
#   starts. The environment block reads the mask actually in force rather than
#   only the flag, so an inherited mask is reported as inherited, the core count
#   is stated as a subset of the online CPUs, and a "cores in use" line names
#   the core types the mask really covers whenever those differ from the
#   machine's. On a host whose topology cannot be read at all, they do not
#   differ and that line is absent.
#
#   An inherited mask is also re-applied to the launch, because tmux forks the
#   measured process from the tmux server rather than from this script, and the
#   server does not carry the mask. Without that, a run started under
#   `taskset -c 0-2` measured a process spread across every core while the block
#   certified three. The bare-metal caveat below therefore applies to an
#   inherited mask exactly as it does to -c.
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

case "$DURATION" in
  ''|*[!0-9]*) echo "error: -d takes whole seconds, got '$DURATION'" >&2; exit 2 ;;
esac
# A fractional -d is worse than a rejected one: GNU sleep accepts 0.5, but the
# window is then shorter than the clock's usable resolution and the result is
# silently wrong rather than absent.
[ "$DURATION" -ge 1 ] || { echo "error: -d must be at least 1 second" >&2; exit 2; }

# Intervals are whole seconds because that is what all-smi's -i takes. Checking
# them here also keeps a value out of the command string tmux hands to a shell,
# and globbing is disabled for the walk so a bare -i '*' cannot turn filenames
# in the working directory into intervals.
set -f
for iv in $INTERVALS; do
  case "$iv" in
    ''|*[!0-9]*) echo "error: -i takes whole seconds, got '$iv'" >&2; exit 2 ;;
  esac
  [ "$iv" -ge 1 ] || { echo "error: -i values must be at least 1, got '$iv'" >&2; exit 2; }
done
set +f

# Wall clock for the window. `date +%s` truncates at both ends, so a 60s window
# can measure as 59 or 61: a 1.7% error sitting on top of the very spread that
# -r exists to expose. bash 5's EPOCHREALTIME avoids it where available, and
# normalises the comma some locales use as the decimal separator.
now_seconds() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    printf '%s\n' "${EPOCHREALTIME/,/.}"
  else
    date +%s
  fi
}

# Kill only the sessions this invocation created. Without this an interrupt
# during a window leaves an orphan rendering a 200x50 TUI forever, which then
# competes for CPU with the operator's next run, and under -c for exactly the
# cluster they pinned, biasing the numbers silently.
cleanup() {
  local s
  for s in $(tmux list-sessions -F '#{session_name}' 2>/dev/null |
             grep "^all_smi_bench_$$_" || true); do
    tmux kill-session -t "$s" >/dev/null 2>&1 || true
  done
}
# INT and TERM must exit, not merely clean up: a bash trap handler returns to
# the point of interruption, so cleaning up without exiting would kill the
# window in flight and then cheerfully open the next one. cleanup is idempotent,
# so the EXIT trap running it a second time is harmless.
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

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

# The affinity mask this run will use, and the machine it sits inside.
#
# A mask does not have to come from -c. A container cpuset, a systemd
# AllowedCPUs= setting, a batch scheduler that pins jobs, or simply invoking
# this script under taskset all narrow the mask before the script starts. The
# signals used below disagree about whether they can see that: nproc honours
# sched_getaffinity and reports the narrowed count, while lscpu, /proc/cpuinfo
# and the sysfs cpu walk all report the whole machine regardless. Reading the
# mask once, here, and scoping each line of the environment block against it is
# what stops those two views from contradicting each other further down.

# Expand "0-2,5,7-8" to "0 1 2 5 7 8", so a mask can be membership-tested
# against the enumerations that do not know about it.
cpu_list_expand() {
  awk -v s="$1" 'BEGIN {
    n = split(s, part, ",")
    for (i = 1; i <= n; i++) {
      if (part[i] == "") continue
      if (split(part[i], r, "-") == 2) {
        for (c = r[1] + 0; c <= r[2] + 0; c++) { printf "%s%d", sep, c; sep = " " }
      } else {
        printf "%s%d", sep, part[i] + 0; sep = " "
      }
    }
    printf "\n"
  }'
}

# Keep the CPU list out of a `taskset -pc` readback, or nothing when what came
# back is not one.
#
# Callers run taskset under LC_ALL=C because util-linux translates this
# message. Under de_DE or ko_KR nothing matches `list:`, the strip is a no-op,
# and the whole sentence survives as the mask. Nothing downstream notices:
# awk reads an unparseable field as 0, so `taskset -c 15` under ko_KR expanded
# to cpu 0 and described a 3.90GHz X925 run as a 2.81GHz A725 one, on the exact
# lines this block exists to make trustworthy. The shape test is the same guard
# from the other side, for a reworded or reformatted readback that LC_ALL=C
# cannot help with: a value that is not a bare CPU list is dropped, so the mask
# reads as unreadable rather than as confidently wrong.
affinity_list() {
  local list
  list="$(sed 's/.*list: *//')"
  case "$list" in
    ''|*[!0-9,-]*) return 0 ;;
  esac
  printf '%s\n' "$list"
}

# CPUs the kernel has online. nproc cannot answer this: it is itself narrowed
# by the mask, which is exactly the contradiction being resolved.
ONLINE_CPUS=""
# The mask actually in force. taskset reads it back through sched_getaffinity,
# so this sees a mask from any source rather than only from -c.
EFFECTIVE_CPUS=""
if [ "$OS" = Linux ]; then
  ONLINE_CPUS="$(cat /sys/devices/system/cpu/online 2>/dev/null || true)"
  if command -v taskset >/dev/null 2>&1; then
    EFFECTIVE_CPUS="$(LC_ALL=C taskset -pc $$ 2>/dev/null | affinity_list || true)"
  fi
fi

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
      effective="$(taskset -c "$CPUSET" sh -c 'LC_ALL=C taskset -pc $$' 2>/dev/null |
                   affinity_list || true)"
      if [ -n "$effective" ] && [ "$effective" != "$CPUSET" ]; then
        AFFINITY_DESC="cpus $effective (taskset, requested $CPUSET)"
      else
        AFFINITY_DESC="cpus $CPUSET (taskset)"
      fi
      EFFECTIVE_CPUS="${effective:-$CPUSET}"
      ;;
    Darwin)
      echo "error: -c is not supported on macOS. There is no way to select a specific CPU set: thread_policy_set affinity is a cache-locality hint that Apple Silicon reports as unsupported, and only E-core confinement is reachable at all, via taskpolicy -b, which also changes scheduling priority." >&2
      exit 1
      ;;
  esac
fi

# Collapse a space-separated CPU list into ranges: "3 5 4 6" becomes "3-6".
#
# Held as awk source in a shell variable because awk has no include and both
# groupers below need it, one bucketing by tolerance and one by exact equality.
# Maintaining two copies of an insertion sort and a run-collapse loop invites
# the failure where a fix lands in one grouper and not the other, and the two
# disagree only on the hosts that reach the second path.
AWK_RANGES='
    function ranges(s,   a, n, i, j, t, out) {
      n = split(s, a, " ")
      # One group can merge several input rows, whose CPU runs interleave, so
      # sort numerically before collapsing rather than trusting input order.
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
'

# Report core types, so a reader can tell a heterogeneous host from a uniform
# one. Grouping by maximum frequency separates ARM big.LITTLE clusters and
# Intel P/E cores alike; cpu_capacity is the device-tree fallback for ARM
# systems without cpufreq. Cloud VMs commonly have neither, since frequency
# scaling is the hypervisor's business there and they are not device-tree
# systems either, so two more signals are tried before giving up: the Intel
# hybrid sysfs CPU lists (cpu_core/cpu_atom), then the ARM `CPU part` field in
# /proc/cpuinfo. When none of the four is readable the topology is reported as
# unknown rather than assumed uniform, because assuming uniform is exactly the
# error this block exists to prevent.
#
# With a CPU list in $1, only those CPUs are enumerated, so the caller can ask
# what the mask covers rather than what the machine has. The sysfs walk is
# blind to the mask on its own: it returns all 20 CPUs of a GB10 under
# `taskset -c 0-2`, next to an nproc-derived count of 3.
linux_topology() {
  local src d file cpu key lines="" keep=""
  if [ -n "${1:-}" ]; then
    keep=" $(cpu_list_expand "$1") "
  fi
  if [ -r /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq ]; then
    src=cpufreq
  elif [ -r /sys/devices/system/cpu/cpu0/cpu_capacity ]; then
    src=capacity
  else
    intel_hybrid_topology "$keep" && return
    arm_part_topology "$keep" && return
    echo "unknown (no cpufreq, cpu_capacity, hybrid sysfs, or CPU part)"
    return
  fi

  for d in /sys/devices/system/cpu/cpu[0-9]*; do
    cpu="${d##*/cpu}"
    case "$cpu" in ''|*[!0-9]*) continue ;; esac
    if [ -n "$keep" ]; then
      case "$keep" in *" $cpu "*) ;; *) continue ;; esac
    fi
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
  printf '%s' "$lines" | sort -k1,1nr -k2,2n | awk -v src="$src" -v tol=0.05 "$AWK_RANGES"'
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

# Group "sortkey cpu label" triples by exact equality on sortkey, ascending,
# and print them in the same "heterogeneous: NxLABEL (cpus ...), ..." /
# "uniform: NxLABEL" grammar as the tolerance-bucketed path above. Shared by
# the two signals below. Neither a hybrid sysfs CPU list nor an ARM part ID is
# a measurement with noise to smooth over, unlike a frequency or capacity
# reading, so bucketing by proximity would solve a problem that does not
# exist here and could wrongly merge two genuinely different groups that
# happen to sort near each other.
group_exact() {
  sort -k1,1n -k2,2n | awk "$AWK_RANGES"'
    {
      key = $1 + 0
      label = $0
      sub(/^[^ ]+ [^ ]+ /, "", label)
      if (g == 0 || key != prevkey) { g++; lbl[g] = label; prevkey = key }
      gcnt[g]++
      glist[g] = glist[g] " " $2
    }
    END {
      out = ""
      for (i = 1; i <= g; i++) {
        out = out (i > 1 ? ", " : "") gcnt[i] "x " lbl[i]
        if (g > 1) out = out " (cpus " ranges(glist[i]) ")"
      }
      printf "%s: %s\n", (g > 1 ? "heterogeneous" : "uniform"), out
    }'
}

# Intel hybrid: the kernel publishes which CPUs are P-cores and which are
# E-cores directly, so this is definitive and needs no tolerance bucketing,
# unlike cpuinfo_max_freq, which carries a per-core turbo-binning spread that
# gives a favoured P-core a higher advertised frequency than its siblings.
# Returns 1 (printing nothing) when neither list is readable, or when a mask
# in $1 excludes every CPU named in both, so the caller falls through to the
# next signal instead of reporting an empty group as uniform.
intel_hybrid_topology() {
  local keep="$1" lines="" cpu plist elist
  if [ -r /sys/devices/cpu_core/cpus ]; then
    plist="$(cpu_list_expand "$(cat /sys/devices/cpu_core/cpus)")"
    for cpu in $plist; do
      if [ -n "$keep" ]; then
        case "$keep" in *" $cpu "*) ;; *) continue ;; esac
      fi
      lines="${lines}1 ${cpu} P-core
"
    done
  fi
  if [ -r /sys/devices/cpu_atom/cpus ]; then
    elist="$(cpu_list_expand "$(cat /sys/devices/cpu_atom/cpus)")"
    for cpu in $elist; do
      if [ -n "$keep" ]; then
        case "$keep" in *" $cpu "*) ;; *) continue ;; esac
      fi
      lines="${lines}2 ${cpu} E-core
"
    done
  fi
  [ -n "$lines" ] || return 1
  printf '%s' "$lines" | group_exact
}

# ARM `CPU part` grouping from /proc/cpuinfo. Independent of both cpufreq and
# cpu_capacity, so it resolves topology on hosts that have neither, which is
# exactly the cloud-VM gap this signal exists to close. Grouping is
# exact-equality: a part ID is a fixed registry value, not a measurement, and
# there is no reading noise here for the tolerance bucket above to survive.
#
# Grouping comes entirely from /proc/cpuinfo; lscpu supplies only the label.
# On a host with no lscpu, or one older than util-linux 2.38 (no MODELNAME
# column), the label falls back to the raw part ID (e.g. "part 0xd85"), but
# the groups and their CPU ranges are unaffected. The fallback is per-part-ID
# text rather than one shared placeholder precisely so a missing label can
# never collapse two real groups into one.
#
# Groups come out ordered by part ID, which is a registry value carrying no
# performance ranking, so unlike the cpufreq path above the first group here is
# not the fastest one. On this GB10 the two orderings agree, 0xd85 (X925)
# sorting below 0xd87 (A725), but that is a coincidence of the registry rather
# than something to read meaning into. The label and CPU list carry the
# information; their order does not. Nothing better is available on this path,
# since it is reached only when both signals that could rank the clusters,
# cpufreq and cpu_capacity, are unreadable.
arm_part_topology() {
  local keep="$1" pairs names lines
  pairs="$(awk '
    /^processor/ { cpu = $NF }
    /^CPU part/  { print cpu, $NF }
  ' /proc/cpuinfo 2>/dev/null)"
  [ -n "$pairs" ] || return 1

  names=""
  command -v lscpu >/dev/null 2>&1 &&
    names="$(LC_ALL=C lscpu -e=CPU,MODELNAME 2>/dev/null | tail -n +2)"

  # part is decoded to a decimal sort key by hand, in portable awk without the
  # gawk-only strtonum. Both `sort -n` and awk's numeric coercion stop at the
  # first non-digit, so every "0x.." part ID would otherwise read as the same
  # value 0 and collapse all part IDs into a single group instead of the
  # several real ones.
  lines="$(printf '%s\n' "$pairs" | awk -v keep="$keep" -v names="$names" '
    function hex2dec(s,   i, c, v, n, digit) {
      sub(/^0[xX]/, "", s)
      v = 0
      n = length(s)
      for (i = 1; i <= n; i++) {
        c = tolower(substr(s, i, 1))
        digit = index("0123456789abcdef", c) - 1
        v = v * 16 + digit
      }
      return v
    }
    BEGIN {
      nn = split(names, nlines, "\n")
      for (k = 1; k <= nn; k++) {
        m = split(nlines[k], f, " ")
        if (m < 2) continue
        label = f[2]
        for (j = 3; j <= m; j++) label = label " " f[j]
        namebycpu[f[1]] = label
      }
    }
    {
      cpu = $1; part = $2
      if (keep != "" && index(keep, " " cpu " ") == 0) next
      label = (cpu in namebycpu) ? namebycpu[cpu] : ("part " part)
      print hex2dec(part), cpu, label
    }
  ')"
  [ -n "$lines" ] || return 1

  printf '%s\n' "$lines" | group_exact
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
    Linux) linux_topology "$@" ;;
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
  local mask="${1:-}"
  case "$OS" in
    Darwin) sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown ;;
    Linux)
      # `model name` is an x86 field, and it names the package rather than a
      # cluster, so a mask cannot change what it should say. aarch64
      # /proc/cpuinfo carries `CPU implementer`/`CPU part` ID pairs instead, so
      # the awk prints an empty string and still exits 0, which is why a
      # trailing `|| echo unknown` does not catch it. lscpu decodes those IDs,
      # and on a heterogeneous part prints one `Model name` per core cluster,
      # so join them: which clusters exist is itself worth reporting, because
      # the same work costs different CPU time on each.
      local name
      name="$(awk -F': ' '/model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null)"
      # Under a mask, joining every cluster names cores the run will never
      # touch: the same class of misreport as an affinity line reading
      # "unpinned" for a pinned run. lscpu's per-CPU MODELNAME column
      # (util-linux 2.38+) says which clusters the mask actually covers.
      if [ -z "$name" ] && [ -n "$mask" ]; then
        name="$(lscpu -e=CPU,MODELNAME 2>/dev/null |
                awk -v keep=" $(cpu_list_expand "$mask") " '
                  NR == 1 { next }
                  index(keep, " " $1 " ") == 0 { next }
                  { sub(/^[ \t]*[0-9]+[ \t]+/, "") }
                  !seen[$0]++ { printf "%s%s", (n++ ? " + " : ""), $0 }')"
      fi
      # Whole-machine fallback, for an older lscpu without that column. It
      # over-reports the clusters under a mask, but not silently: the "N of M
      # cores" count printed beside it already says the run is confined.
      #
      # LC_ALL=C because this one matches a field label, and lscpu translates
      # its labels: a de_DE host prints `Modellname`, nothing matches, and the
      # cpu line reads `unknown`. The -e=CPU,MODELNAME path above is unaffected,
      # the column name there being an argument rather than output, so without
      # this the same machine could name its cluster correctly under a mask and
      # not without one.
      if [ -z "$name" ]; then
        name="$(LC_ALL=C lscpu 2>/dev/null | awk -F': +' '
          /^Model name/ { if (seen[$2]++) next; n[++c] = $2 }
          END { for (i = 1; i <= c; i++) printf "%s%s", (i > 1 ? " + " : ""), n[i] }')"
      fi
      echo "${name:-unknown}"
      ;;
  esac
}

# How many cores this process can see, minus nproc's OpenMP manners: it caps its
# answer at OMP_NUM_THREADS/OMP_THREAD_LIMIT, which most ML and HPC images export
# and which say nothing about sched_getaffinity. A thread-count cap is not a core
# count, and printing it as one puts a number beside the topology that
# contradicts it: OMP_NUM_THREADS=4 on an unpinned GB10 read two 10-core clusters
# on the topology line and `4 cores` on the line directly above it.
core_count() {
  case "$OS" in
    Darwin) sysctl -n hw.ncpu 2>/dev/null || echo "?" ;;
    Linux) (unset OMP_NUM_THREADS OMP_THREAD_LIMIT; nproc 2>/dev/null) || echo "?" ;;
  esac
}

ONLINE_COUNT=""
if [ -n "$ONLINE_CPUS" ]; then
  ONLINE_COUNT="$(cpu_list_expand "$ONLINE_CPUS" | wc -w | tr -d ' ')"
fi

# Does the mask cover less than the whole machine? Compare expanded sets rather
# than the two strings, so a difference in range formatting between the kernel's
# online list and taskset's readback cannot masquerade as a restriction.
CPUS_RESTRICTED=false
if [ -n "$EFFECTIVE_CPUS" ] && [ -n "$ONLINE_CPUS" ] &&
   [ "$(cpu_list_expand "$EFFECTIVE_CPUS")" != "$(cpu_list_expand "$ONLINE_CPUS")" ]; then
  CPUS_RESTRICTED=true
fi

# Without taskset the mask cannot be read at all, but nproc is narrowed by it,
# so a count below the online set still proves one is in force. Which CPUs is
# then unknowable from shell, and the affinity line below says exactly that
# rather than falling back to "unpinned", which would be the same misreport in
# a quieter form.
CPUS_RESTRICTED_OPAQUE=false
VISIBLE_CPUS=""
if [ -z "$EFFECTIVE_CPUS" ] && [ -n "$ONLINE_COUNT" ]; then
  # core_count strips the OpenMP cap for the reason given at its definition.
  # Left in, a thread-count cap on an entirely unpinned host would be reported
  # below as an affinity mask.
  VISIBLE_CPUS="$(core_count)"
  case "$VISIBLE_CPUS" in
    ''|*[!0-9]*) ;;
    *) if [ "$VISIBLE_CPUS" -lt "$ONLINE_COUNT" ]; then CPUS_RESTRICTED_OPAQUE=true; fi ;;
  esac
fi

# Passed to the reporting helpers below, and empty unless the mask actually
# narrows the machine, so an unrestricted run takes byte-for-byte the path it
# took before any of this existed.
MASK_ARG=""
if [ "$CPUS_RESTRICTED" = true ]; then
  MASK_ARG="$EFFECTIVE_CPUS"
fi

TOPOLOGY="$(detect_topology)"

# What the mask covers, as opposed to what the machine has. Printed only when
# those differ, since otherwise it would repeat the line above verbatim.
#
# A narrowing mask does not guarantee they differ, because detect_topology is
# not obliged to vary with it. On a host with neither cpufreq nor cpu_capacity
# it returns a fixed "unknown" string whatever it is asked to enumerate, and on
# a host where only some CPUs expose either file, a mask that drops only the
# CPUs that were never counted leaves the string untouched. Both cases reach
# the same answer twice, so compare rather than infer.
TOPOLOGY_IN_USE=""
if [ -n "$MASK_ARG" ]; then
  TOPOLOGY_IN_USE="$(detect_topology "$MASK_ARG")"
  # Not `[ ... ] && TOPOLOGY_IN_USE=""`: that form leaves the enclosing if
  # returning 1 on the common path, which is harmless here and a trap for
  # whoever later wraps this block in a function under set -e.
  if [ "$TOPOLOGY_IN_USE" = "$TOPOLOGY" ]; then
    TOPOLOGY_IN_USE=""
  fi
fi

# On a heterogeneous host an unpinned run is a mix of core types, and the
# reader needs to know that before comparing the number to another machine's.
# The test is whether the cores the run can reach are of more than one type,
# not whether the machine has more than one: a run already confined to a single
# cluster cannot migrate across types, so pointing it at -c would be advice it
# has effectively already taken.
MIXED_CORE_TYPES=false
case "${TOPOLOGY_IN_USE:-$TOPOLOGY}" in
  heterogeneous*) MIXED_CORE_TYPES=true ;;
esac

if [ -z "$CPUSET" ]; then
  if [ "$CPUS_RESTRICTED" = true ]; then
    # An inherited mask is invisible to -c, so this line used to read
    # "unpinned" for a run confined to part of the machine: the opposite of
    # what happened, on the one line whose job is to certify core placement.
    AFFINITY_DESC="cpus $EFFECTIVE_CPUS (inherited from the environment, not -c"
    if [ "$MIXED_CORE_TYPES" = true ]; then
      AFFINITY_DESC="$AFFINITY_DESC; mixed core types: threads may migrate"
    fi
    AFFINITY_DESC="$AFFINITY_DESC)"
    # Re-apply the mask to the launch, or the line above certifies a placement
    # the measured process does not have. tmux does not fork the pane from this
    # script: it forks it from the tmux server, which on the shared default
    # socket was started by some earlier, unrestricted shell and hands the pane
    # its own mask instead. Verified: against a server already running,
    # `taskset -c 0-2 tmux new-session` yields a pane whose mask is the
    # server's 0-19. Under -c the prefix is set for this same reason, which is
    # why that path never had the problem. Where the pane would have inherited
    # the mask anyway this is a no-op, and EFFECTIVE_CPUS has been shape-checked
    # to a bare CPU list before it reaches the command string tmux hands to a
    # shell.
    LAUNCH_PREFIX="taskset -c $EFFECTIVE_CPUS "
  elif [ "$CPUS_RESTRICTED_OPAQUE" = true ]; then
    AFFINITY_DESC="restricted, cpus unknown (inherited from the environment; install util-linux taskset to report which)"
  elif [ "$MIXED_CORE_TYPES" = true ]; then
    # Only Linux is pointed at -c: every Apple Silicon Mac reports two
    # perflevels and so lands here, and telling those runs to use a flag that
    # macOS rejects would be advice that never works.
    case "$OS" in
      Linux) AFFINITY_DESC="unpinned (mixed core types: threads may migrate, see -c)" ;;
      Darwin) AFFINITY_DESC="unpinned (mixed core types: threads may migrate; macOS has no CPU affinity control)" ;;
    esac
  fi
fi

# How many cores the measured run gets, and what that is a subset of. Count the
# effective mask rather than calling nproc: nproc reports the mask of *this*
# process, which under -c is not the mask the measured process will be launched
# with. Pinning to ten CPUs from an unrestricted shell would otherwise print
# "20 of 20 cores" directly above a "cores in use" line reading 10x.
#
# Where the mask is real but unreadable, the count is the one the restriction
# probe already established. Recomputing it here is the same call, and reusing
# it keeps the number that decides "restricted" and the number printed beside
# that word from ever being two different numbers.
if [ "$CPUS_RESTRICTED" = true ]; then
  CPU_COUNT="$(cpu_list_expand "$EFFECTIVE_CPUS" | wc -w | tr -d ' ') of $ONLINE_COUNT"
elif [ "$CPUS_RESTRICTED_OPAQUE" = true ]; then
  CPU_COUNT="$VISIBLE_CPUS of $ONLINE_COUNT"
else
  CPU_COUNT="$(core_count)"
fi

echo "=== environment ==="
printf '  all-smi       %s\n' "$("$BIN" --version 2>/dev/null | head -1 || echo unknown)"
printf '  binary        %s\n' "$BIN"
printf '  os            %s %s (%s)\n' "$OS" "$(uname -r)" "$(uname -m)"
printf '  cpu           %s (%s cores)\n' "$(detect_cpu "$MASK_ARG")" "$CPU_COUNT"
printf '  topology      %s\n' "$TOPOLOGY"
if [ -n "$TOPOLOGY_IN_USE" ]; then
  printf '  cores in use  %s\n' "$TOPOLOGY_IN_USE"
fi
printf '  affinity      %s\n' "$AFFINITY_DESC"
printf '  gpu           %s\n' "$(detect_gpu)"
printf '  processes     %s\n' "$(($(ps -A 2>/dev/null | wc -l) - 1))"
printf '  terminal      %sx%s (tmux)\n' "$TMUX_COLS" "$TMUX_ROWS"
printf '  window        %ss measured after %ss warmup%s\n' "$DURATION" "$WARMUP_SECS" \
  "$([ "$REPEATS" -gt 1 ] && printf ', %s repeats' "$REPEATS" || true)"
echo

BIN_NAME="$(basename "$BIN")"

# Basename of a running process, portably: Linux `ps -o comm=` gives the bare
# name, macOS can give a full path.
process_name() { ps -o comm= -p "$1" 2>/dev/null | tr -d ' ' | sed 's#.*/##'; }

# The PID running in the session's pane. tmux execs the command in place, so
# for both `all-smi ...` and `taskset -c LIST all-smi ...` this is the binary
# itself, verified on both forms. If tmux ever wraps the command in a shell,
# the binary is that shell's child, so fall back to looking one level down.
#
# Asking tmux which process it started is what makes the measurement immune to
# a second all-smi appearing meanwhile. Diffing `pgrep` snapshots, as this did
# before, would silently attach to whichever new PID sorted first, and this
# script now actively invites concurrent runs by telling operators to pin one
# cluster at a time.
session_pid() {
  local session="$1" pane child
  pane="$(tmux list-panes -t "$session" -F '#{pane_pid}' 2>/dev/null | head -1)"
  [ -n "$pane" ] || return 1
  if [ "$(process_name "$pane")" = "$BIN_NAME" ]; then
    printf '%s\n' "$pane"
    return 0
  fi
  child="$(pgrep -P "$pane" -x "$BIN_NAME" 2>/dev/null | head -1)"
  [ -n "$child" ] || return 1
  printf '%s\n' "$child"
}

# Run one measurement window. Echoes "cpu_seconds wall_seconds rss_kb" on
# success. Failures are reported through the exit code so the caller can name
# the reason after all repeats have run: 1 could not start, 2 could not read
# CPU time, 3 exited early, 4 window was not positive.
measure_once() {
  local label="$1" rep="$2"; shift 2
  local session="all_smi_bench_$$_${label//[^0-9a-zA-Z]/_}_${rep}"

  tmux kill-session -t "$session" >/dev/null 2>&1 || true
  # Keep stderr: a failure to launch is otherwise reported as a bare "could not
  # start" after 10s of polling, with the actual reason discarded. stdout is
  # dropped because this function's stdout is parsed as numbers by the caller.
  local launch_err
  launch_err="$(tmux new-session -d -s "$session" -x "$TMUX_COLS" -y "$TMUX_ROWS" \
    "${LAUNCH_PREFIX}$BIN local $*" 2>&1 >/dev/null)" || true

  local pid=""
  for _ in $(seq 1 20); do
    sleep 0.5
    pid="$(session_pid "$session" || true)"
    [ -n "$pid" ] && break
  done
  if [ -z "$pid" ]; then
    [ -n "$launch_err" ] && printf 'note: tmux: %s\n' "$launch_err" >&2
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    return 1
  fi

  sleep "$WARMUP_SECS"

  local t0 c0 t1 c1 rss
  c0="$(cpu_time_seconds "$pid")" ||
    { tmux kill-session -t "$session" >/dev/null 2>&1 || true; return 2; }
  t0="$(now_seconds)"
  sleep "$DURATION"
  c1="$(cpu_time_seconds "$pid")" ||
    { tmux kill-session -t "$session" >/dev/null 2>&1 || true; return 3; }
  t1="$(now_seconds)"

  rss="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
  [ -n "$rss" ] || rss=0

  tmux kill-session -t "$session" >/dev/null 2>&1 || true

  awk -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" -v rss="$rss" 'BEGIN {
    wall = t1 - t0
    if (wall <= 0) exit 1
    printf "%.6f %.3f %d\n", c1 - c0, wall, rss
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
               label, mean, ct, wall + 0.5, rss / 1024
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
