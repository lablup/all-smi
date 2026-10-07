# Monitoring guide

[Home](../README.md) · [Documentation](../README.md#documentation)

## Local Mode (Monitor Local Hardware)

The `local` mode monitors your local GPUs/NPUs with a terminal-based interface. This is the default when no command is specified, unless `general.default_mode` is changed in configuration.

```bash
# Monitor local GPUs
all-smi              # Default to local mode
all-smi local        # Explicit local mode

# With custom refresh interval
all-smi local --interval 5
```

## Remote View Mode (Monitor Remote Nodes)

The `view` mode monitors multiple remote systems that are running in API mode. This mode requires specifying remote endpoints.

```bash
# Direct host specification (required)
all-smi view --hosts http://gpu-node1:9090 http://gpu-node2:9090
all-smi view --hosts http://gpu-node1:9090,http://gpu-node2:9090

# Using host file (required)
all-smi view --hostfile hosts.csv --interval 2
```

**Note:** HTTP view mode needs endpoints from `--hosts`, `--hostfile`, or configuration. SSH and replay use their own inputs. `--hosts` accepts endpoints separated by spaces, commas, or a mixture of both; explicit `http://` and `https://` schemes are preserved. Invalid endpoint syntax is reported before the TUI starts. For local monitoring, use `all-smi local` instead.

Host file format (CSV):
```
http://gpu-node1:9090
http://gpu-node2:9090
http://gpu-node3:9090
```

## Cluster Management

> Note: The Cluster Overview Dashboard, Live Statistics History, and Tabbed Interface appear in remote views (HTTP/SSH) and replay. Local mode replaces these with a compact two-line host summary bar showing hostname, CPU model, architecture, uptime, and live sparkline metrics (CPU%, GPU%, RAM, power, temperature).

- **Cluster Overview Dashboard:** Real-time statistics showing:
  - Total nodes and GPUs across the cluster
  - Average utilization and memory usage
  - Temperature statistics with standard deviation
  - Total and average power consumption
  - Per-node LED grid (rendered beside the overview cards): one dot per node, colored by GPU utilization, with filled/hollow/crossed symbols for selected/connected/disconnected states
- **Live Statistics History:** Full-width braille sparkline panel showing GPU and CPU utilization, memory, and temperature side by side
- **Tabbed Interface:** Switch between "All" view and individual host tabs
- **Adaptive Update Intervals:**
  - Local monitoring: 1 second (Apple Silicon) or 2 seconds (others)
  - 1-10 remote nodes: 3 seconds
  - 11-50 nodes: 4 seconds
  - 51-100 nodes: 5 seconds
  - 101+ nodes: 6 seconds

## Agentless SSH mode

`all-smi view --ssh user@host[,user@host2,...]` connects to one or more
remote machines over SSH and renders their metrics in the same TUI,
**without** first installing or starting `all-smi api` on the targets.

On first connect the transport probes each host, in order:

1. `all-smi snapshot --format json` — used when the binary is present
   and at least v0.22. The tab chip shows `native`.
2. `nvidia-smi --query-gpu=...` — CSV fallback for NVIDIA boxes without
   `all-smi`. Chip shows `nvidia-smi`.
3. `rocm-smi --json` — JSON fallback for AMD boxes. Chip shows
   `rocm-smi`.
4. Otherwise the host is marked `unsupported`.

Quick start:

```bash
# Monitor two DGX boxes over SSH, using a local private key.
all-smi view --ssh admin@dgx-01,admin@dgx-02

# Bulk mode from a hostfile (see examples/hosts-ssh.txt).
all-smi view --ssh-hostfile examples/hosts-ssh.txt

# Accept unknown host keys on first connect (TOFU) and persist them.
all-smi view --ssh admin@new-node --ssh-strict-host-key accept-new
```

SSH agent authentication and passphrase prompting are not implemented. The key must be loadable without a passphrase. `--ssh-config` is accepted but currently ignored; use explicit connection flags. See [configuration limitations](configuration.md#limitations) before overriding SSH policy through TOML.

Key flags:

| Flag | Default | Notes |
| --- | --- | --- |
| `--ssh user@host[:port][,...]` | — | Comma-separated SSH targets. |
| `--ssh-hostfile <path>` | — | One `user@host[:port]` per line; `#` comments allowed. |
| `--ssh-key <path>` | auto-probe | Overrides `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa` discovery. |
| `--ssh-strict-host-key yes\|accept-new\|no` | `yes` | Matches OpenSSH semantics. |
| `--ssh-timeout-secs <n>` | `10` | Per-target TCP/handshake timeout. |
| `--ssh-fallback nvidia-smi,rocm-smi,none` | both enabled | Which shim(s) to try when `all-smi` is absent. |
| `--ssh-known-hosts <path>` | `~/.ssh/known_hosts` | Custom known-hosts file. |
| `--ssh-concurrency <n>` | `32` | Bound on concurrent SSH connects (semaphore-limited). |

Security notes:

- Password auth is **never** attempted; local private-key authentication only. No
  password ever flows through the CLI or logs.
- The SSH command string emitted on the wire is fixed per transport and
  does not interpolate remote input into shell commands.
- `--ssh-strict-host-key=no` logs a prominent TUI warning so a
  misconfiguration is obvious to the operator.
- `known_hosts` writes use `O_NOFOLLOW` and reject a pre-existing
  symlink at the target path; an attacker who controls the directory
  cannot redirect host-key lines into an arbitrary file (e.g.
  `~/.bashrc`). If persistence fails, accepted keys are kept in an
  in-process cache so a subsequent connection in the same run can still
  detect a key change.

## Interactive UI
- **Enhanced Controls:**
  - Keyboard: Arrow keys, Page Up/Down, Tab switching
  - Mouse: Click column headers to sort (process view)
  - Sorting: 'd' (default), 'u' (utilization), 'g' (GPU memory), 'p' (PID), 'm' (memory), 'c' (CPU)
  - Filtering: 'f' (toggle GPU process filter - show only processes with GPU memory usage)
  - Query filter: '/' (open query bar), 'Ctrl-R' (recall last query), 'ESC' (clear)
  - Alerts: 'A' (toggle alert history panel)
  - Users tab: 'V' (jump to cluster-wide user aggregation tab)
  - Interface: '1'/'h' (help), 'q' (quit), ESC (close help)
- **Visual Design:**
  - Color-coded utilization and thermal status
  - Per-column coloring in process view
  - Responsive layout adapting to terminal size
  - Double-buffered rendering for flicker-free display
- **Help System:** Context-sensitive help with all keyboard shortcuts

## Cluster-Wide Users Tab (`V`)

Remote `view` mode adds a **Users** tab that aggregates per-process metrics
across every scraped host so operators can answer "who is using the cluster
and how much?" at a glance. Enable per-host process collection with
`all-smi api --processes` on every node, then press `V` in the remote view to
jump to the tab (it sits right after `All` in the tab row and is cycled by the
arrow keys).

Columns:

| Column | Meaning |
| --- | --- |
| `USER` | Username from the per-process metrics (`?` when a host emits rows without `user` labels, e.g. Windows API mode) |
| `NODES` | Distinct hosts the user has at least one process on |
| `GPUs` | Distinct `(host, gpu_index)` pairs the user touches |
| `PROCS` | Distinct `(host, pid)` pairs — the same PID on two hosts counts as two processes |
| `VRAM` | Sum of GPU memory across all of the user's processes |
| `POWER*` | Weighted power approximation (see below) |
| `LONGEST` | Oldest `TIME+` value across the user's processes |
| `CMD (top-1 by GPU mem)` | Command owning the largest VRAM row |

**Power approximation.** `POWER*` is computed as

```
sum_over_gpus(
  gpu.power × (user_vram_on_gpu / total_vram_on_gpu_across_all_users)
)
```

per GPU the user touches, summed across GPUs. The formula is an
approximation because `nvidia-smi`/NVML does not report per-process power
directly; we proxy it with the user's share of reported process VRAM on each GPU. Negative input power is clamped to zero. The `*` in the header marks the column as approximate; this is not a per-process power measurement or a billing value.

**In-tab keybindings**

- `u` sort by username (default)
- `m` sort by total GPU memory
- `p` sort by total power (derived)
- `n` sort by node count
- `t` sort by oldest process start time (`LONGEST`)
- `Enter` drill down into the highlighted user (per-host breakdown)
- `Enter` again drills into the host for the selected user (process list)
- `ESC` exits drill-down (ESC outside drill-down returns to normal handling)
- `f` toggles the system-account filter (hides `root`/`uid<1000` by default)
- `e` exports the current visible table to
  `<cache>/all-smi/users-<timestamp>.csv` (the platform cache directory
  — see [cache paths](configuration.md#cache-paths))

**Partial coverage.** When some hosts report `--processes` data and others
don't, the tab shows a yellow chip `⚠ partial coverage: M of N nodes reporting
process data` so operators don't misread the numbers. If zero hosts report
process data, the tab renders a hint pointing at the `--processes` flag
instead of an empty table.


## Topology View (`T`)

Remote and replay `view` modes add a **Topology** tab that visualises the
selected host's intra-node GPU interconnect: NvLink connections
(GPU↔GPU, GPU↔NvSwitch), NUMA affinity, and PCIe lanes. Press `T` to
jump to the tab (it sits right after `Users`); use the arrow keys
(`Left`/`Right`) to cycle between hosts while the Topology tab is active.
The tab remembers the host you last selected so pressing `T` returns to
the same node instead of snapping to the first one in the strip.

Two render modes are available; press `M` to toggle between them:

- **Graph mode** (default) — ASCII layout showing NUMA zones as boxes
  with GPUs inside and NvLink / NvSwitch edges drawn between them.
  NUMA boxes stack side-by-side on wide terminals and fall back to
  vertical stacking on narrower ones.
- **Matrix mode** — `nvidia-smi topo -m`-equivalent table with CPU
  affinity and NUMA columns. Uses the same vocabulary: `X`=self,
  `NVn`=NvLink Gen-n, `NSW`=NvSwitch, `PXB`=PCIe bridge, `NODE`=same
  NUMA, `SYS`=across NUMA.

**Graceful degradation.** The tab is designed to produce useful output
on every platform:

- Hosts without NvLink render only NUMA + PCIe groupings.
- Non-NVIDIA hosts omit the `NVn`/`SYS` vocabulary and show NUMA groups
  only.
- Hosts without NUMA topology render a single synthetic `NUMA ?` box.
- When the terminal is narrower than 100 columns the graph renderer
  drops to matrix mode automatically so the content never overflows on
  80-column sessions.

**Bandwidth hints.** When the exporter provides a per-link bandwidth
(`bandwidth_mb_s` label on `all_smi_nvlink_remote_device_type`), the
matrix derives the NVn generation from it (e.g. 50 GB/s → `NV5`).
Without the hint the renderer falls back to a generic `NV` label so no
hallucinated generation reaches the operator.


## Filtering & Alerts

Press `/` in any tab to open the filter query bar and hide/dim GPUs that do not
match a small DSL. The query compiles once and is evaluated per row per frame,
so the filter stays active across refreshes and tab switches until you clear
it with `ESC`.

**Filter DSL**

- Fields: `temp`, `util`, `mem_pct`, `mem_used`, `mem_total`, `power`, `user`,
  `host`, `gpu_name`, `driver`, `index`, `uuid`, `pstate`, `numa`,
  `device_type`.
- Numeric operators: `>`, `>=`, `<`, `<=`, `==`, `!=`.
- String operators: `==`, `!=`, `~=` (regex, size-bounded to 128 KiB).
- Combine with `&` / `|` and parenthesise with `(...)`.
- Unknown field names are a parse error; fields that a device does not expose
  (e.g. `temp` on a CPU row) make the row *not match* so mixed views stay
  readable.

Examples:

```text
/                               # open the bar
temp>85                         # GPUs over 85 °C
util<5 & power>300              # idling but still drawing power
host~=dgx                       # only dgx-* nodes
user==alice | user==bob         # either user
(temp>80 | util>90) & numa==0   # hot-or-busy GPUs on NUMA 0
```

Press `Enter` to commit, `Ctrl-R` to cycle the last five queries, and `ESC`
to clear. Invalid queries surface an inline red `parse error: ... at col N`
message without committing.

**Threshold alerts**

Alert settings follow the standard precedence order: CLI flags, environment
variables, the `[alerts]` section of the TOML config file, then built-in
defaults. The CLI exposes direct overrides for warning temperature and idle
duration:

```bash
all-smi local --alert-temp 75 --alert-util-low-mins 10
all-smi view --hosts http://n01:9090 --alert-temp 75
```

When a GPU crosses a threshold, all-smi emits:

- A 5-second toast in the status bar.
- A 2-second red border flash on the affected GPU card.
- An entry in an in-memory ring buffer (last 50 transitions).

Press `A` to toggle the alert history panel, `ESC` to close it. When the
`[alerts] webhook_url` option is set, each transition is POSTed to the
configured URL as `{timestamp, host, gpu_index, rule, from, to, value,
threshold}` JSON with a 2-second timeout, fire-and-forget.


Webhook redirects are not followed. Configure the final destination URL directly. CSV exports use owner-only files on Unix and escape spreadsheet formula prefixes. See [implementation safeguards](ARCHITECTURE.md#tui-input-and-export-safeguards).

## Reading metrics

- GPU/NPU cards show utilization, memory, temperature, clocks, and power where the reader supports them. NVIDIA-specific views can include MIG, vGPU, PCIe, and NvLink details.
- CPU cards show utilization, core/thread counts, clocks, and available temperature/power readings; Apple Silicon exposes P/E cluster information.
- Memory cards show RAM and swap. The swap row is hidden when no swap is configured; active swap is highlighted in red.
- Process views show available PID, command, user, CPU, and GPU memory information, with sorting and GPU-process filtering.
- Chassis readings are platform-dependent estimates, not a universal wall-power meter. On generic Linux hosts, power can be an aggregate GPU reading and inlet/outlet temperatures are the minimum/maximum readable thermal zones, not dedicated BMC inlet/outlet probes. Intel Macs use SMC estimates; Apple Silicon exposes CPU/GPU/ANE components.

See [hardware requirements](installation.md) and the [metric reference](../API.md) for availability.

## Energy and cost

The energy row shows session energy and estimated cost. Press `R` to reset the session without resetting the lifetime Prometheus counter `all_smi_energy_consumed_joules_total`. API-mode WAL persistence preserves accumulated energy across restarts when its path is writable. Configure price, currency, and WAL behavior in [`[energy]`](configuration.md#schema); this is an estimate based on reported power, not billing-grade metering.
