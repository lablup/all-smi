# Diagnostics and troubleshooting

[Home](../README.md) · [Documentation](../README.md#documentation)

The `all-smi doctor` subcommand runs a read-only suite of environment checks and
prints a PASS/WARN/FAIL report covering platform, privileges, container
runtime, every supported hardware backend (NVIDIA, AMD, Apple, Gaudi, TPU,
Tenstorrent, Rebellions, Furiosa, AWS Neuron, Intel Level Zero, Windows), the relevant
environment variables, and optional remote endpoint connectivity. Each check
has a hard 3-second timeout.

The `level_zero.*` checks report the four stages that decide whether Intel GPU
temperature, power, and frequency are collectable at all, separately rather
than as one verdict: whether the backend is compiled in, which loader library
was found, whether the runtime initialised and by which Sysman route, and how
many devices Sysman enumerated. None of them can FAIL, because an absent Level
Zero runtime degrades to the sysfs or WMI baseline rather than breaking a run,
and a host with no Intel GPU reports SKIP rather than complaining about
hardware it does not have.

```bash
# Human-readable report (default)
all-smi doctor

# Machine-readable JSON for CI and scripts
all-smi doctor --json

# Support bundle for attaching to GitHub issues
all-smi doctor --bundle report.tar.gz

# Keep hostnames / IPs / MAC / usernames (default scrubs them)
all-smi doctor --bundle report.tar.gz --include-identifiers

# Run only a subset of checks
all-smi doctor --only platform,privileges

# Skip specific checks (prefix match)
all-smi doctor --skip nvidia.mig.mode

# Probe remote endpoints (DNS, TCP, HTTP /metrics)
all-smi doctor --remote-check http://gpu-node1:9090
```

Exit codes:

- `0` — every check passed (or skipped)
- `1` — at least one check returned WARN
- `2` — at least one check returned FAIL

The `NO_COLOR` environment variable is respected for CI log readability.

### Support Bundle Security

When `--bundle <PATH>` is used, the archive is written with the following
hardening on Unix:

- **Symlink refusal** — the output file is opened with `O_NOFOLLOW`. A
  pre-existing symlink at `<PATH>` causes the command to fail with an error
  rather than following the link (e.g., into `/etc/shadow`).
- **Owner-only permissions** — the file is created with mode `0600` so only
  the invoking user can read or write it.
- **Secret-value redaction** — any environment variable whose name contains a
  known credential keyword (`TOKEN`, `SECRET`, `PASSWORD`, `API_KEY`,
  `ACCESS_KEY`, `PRIVATE_KEY`, `CREDENTIAL`, `AUTH`, `SESSION`, `COOKIE`,
  `BEARER`, `SIGNATURE`, `ENCRYPTION_KEY`, `CLIENT_SECRET`) has its value
  replaced with `<redacted:secret>` in `env.txt`. This redaction is always
  applied, even when `--include-identifiers` is set.
- **`--include-identifiers`** — by default the bundle scrubs hostnames, IPv4,
  IPv6, MAC addresses, and the current username from all text files. Passing
  `--include-identifiers` opts back in to those network-identity tokens only.
  Credential values (above) are **never** restored by this flag.

Stable check IDs (greppable across versions):

| Category | Example IDs |
|---|---|
| `platform.*` | `platform.os`, `platform.runtime`, `platform.cpu`, `platform.cpu_temp`, `platform.memory`, `platform.hardware`, `platform.uptime` |
| `privileges.*` | `privileges.user`, `privileges.root`, `privileges.video_render_group`, `privileges.dev_dri`, `privileges.dev_tenstorrent` |
| `container.*` | `container.runtime`, `container.cgroup`, `container.k8s_serviceaccount` |
| `nvidia.*` | `nvidia.nvml.loadable`, `nvidia.smi.binary`, `nvidia.driver.version`, `nvidia.env.visible_devices`, `nvidia.mig.mode` |
| `amd.*` | `amd.rocm.version`, `amd.libamdgpu_top.abi`, `amd.gpu_metrics.revision`, `amd.dri.perms`, `amd.build.target_env`, `amd.adl.library`, `amd.adl.sensors`, `amd.adl.adapters`, `amd.adl.per_adapter` |
| `apple.*` | `apple.macos.version`, `apple.silicon`, `apple.smc` |
| `gaudi.*` | `gaudi.hlsmi`, `gaudi.devices`, `gaudi.driver` |
| `tpu.*` | `tpu.libtpu`, `tpu.env.name`, `tpu.accel.vendor` |
| `tenstorrent.*` | `tenstorrent.luwen`, `tenstorrent.kmd`, `tenstorrent.module` |
| `rebellions.*` | `rebellions.rblnstat`, `rebellions.driver` |
| `furiosa.*` | `furiosa.feature`, `furiosa.smi` |
| `neuron.*` | `neuron.dev_node`, `neuron.driver`, `neuron.sysfs`, `neuron.tools` |
| `windows.*` | `windows.wmi`, `windows.amd_ryzen_master`, `windows.intel_wmi`, `windows.libre_hardware_monitor` |
| `env.*` | `env.all_smi`, `env.cuda`, `env.rocr`, `env.tpu`, `env.hl` |
| `network.*` | `network.dns`, `network.tcp`, `network.http` |

## NVML initialisation failures and back-off

all-smi shares one NVML handle for the whole process and keeps `libnvidia-ml` loaded until exit. When NVML cannot initialise (driver missing or mismatched, device nodes not visible in a container or a sandboxed service), all-smi reports the error, reads GPU data from `nvidia-smi` instead, and does not call NVML init again until a back-off expires. The back-off starts at 30 seconds, doubles after each failed attempt up to 5 minutes, and resets after a successful init. A handle that initialises but then fails `device_count()` is replaced once immediately; if the replacement is also dead, the same back-off applies.

After you fix the cause, expect NVML data to return within 5 minutes without restarting all-smi. Restarting the process retries at once. `all-smi doctor` (`nvidia.nvml.loadable`) shows the underlying NVML error and a fix hint.

## Wrong or missing CPU temperature on Linux

all-smi picks the CPU temperature file once per process, at the first sample, and then reads only that file. The first readable candidate in this order wins:

1. The `ALL_SMI_CPU_TEMP_SENSOR` override, if set (see below).
2. A CPU driver by hwmon `name`: `coretemp`, `k10temp`, `zenpower`, `cpu_thermal`, `cpu-thermal`.
3. A CPU thermal zone by `type`: `x86_pkg_temp`, `cpu-thermal`, `cpu_thermal`, `CPU-therm`.
4. A motherboard sensor chip input (Super I/O, board EC or WMI drivers such as `nct6775`, `nct6683`, `asus_ec_sensors`, `asus_wmi_sensors`, `dell_smm`) whose `temp*_label` is exactly one of, in priority order: `CPU Package`, `PECI Agent 0`, `PECI Agent 0 Calibration`, `PECI 0.0`, `CPU Temperature`, `CPU`, `CPUTIN`. The best label wins across all chips; ties go to the lower hwmon index. Labels such as `CPU Socket Temperature` or `CPU VRM Temperature` are never taken.
5. The legacy paths `thermal_zone0`, `thermal_zone1`, `hwmon0/temp1_input`, `hwmon1/temp1_input`.

`all-smi doctor --only platform.cpu_temp` shows which rule matched, the driver and label, the file, and the current reading:

```text
PASS platform.cpu_temp                board-label nct6798 "PECI Agent 0" at /sys/class/hwmon/hwmon2/temp7_input, 48°C
```

If the value looks stuck or wrong (often a motherboard `thermal_zone0` reading), or the chip has no labels (`it87`, `gigabyte_wmi`, `f71882fg`, where the CPU input depends on board wiring), name the sensor explicitly with `ALL_SMI_CPU_TEMP_SENSOR`:

```bash
# Input 2 of the first it87 chip that reads
ALL_SMI_CPU_TEMP_SENSOR=it87:temp2 all-smi
# The input labelled exactly CPUTIN on an nct6798 chip
ALL_SMI_CPU_TEMP_SENSOR=nct6798:CPUTIN all-smi
# An absolute path to a temp*_input or a thermal zone temp file under /sys
ALL_SMI_CPU_TEMP_SENSOR=/sys/class/hwmon/hwmon3/temp2_input all-smi
```

The `<driver>:...` forms search every hwmon device with that `name`, lowest index first, so they survive hwmon renumbering across reboots; prefer them over a path. Label matching is exact and case-sensitive. An empty value counts as unset. A malformed value, a path outside `/sys`, or a sensor that matches nothing readable is ignored: all-smi continues with automatic selection, and `all-smi doctor` reports the rejection as a warning, for example ``ALL_SMI_CPU_TEMP_SENSOR=it87:temp2 ignored: no hwmon device is named `it87` ``. The variable is read once at startup, so restart all-smi after changing it.

To find the right input, compare `sensors` (lm-sensors) with the raw sysfs files:

```bash
grep . /sys/class/hwmon/hwmon*/name
grep . /sys/class/hwmon/hwmon*/temp*_label
grep . /sys/class/hwmon/hwmon*/temp*_input   # millidegrees Celsius
```

Load the CPU under a stress test and pick the input that rises with it.
