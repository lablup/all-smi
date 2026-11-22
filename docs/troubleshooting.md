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
3. A CPU thermal zone by `type`: first `x86_pkg_temp`, `cpu-thermal`, `cpu_thermal`, `CPU-therm`, and only when none of those reads, the ARM SoC types `package-thermal`, `cpu0-thermal`, `soc-thermal`, `soc_thermal`, `BCPU-therm`. Within each group, zones are tried in zone index order. On RK3588 boards without `package-thermal`, `bigcore0-thermal` (the first big-core cluster) is used, but only when no zone of the other types reads. Other cluster zones such as `bigcore2-thermal` or `littlecore-thermal` are never taken.
4. A motherboard sensor chip input (Super I/O, board EC or WMI drivers) whose `temp*_label` is exactly one of the labels in the table below, in priority order. The best label wins across all chips; ties go to the lower hwmon index, then the lower input number. `asus_wmi_sensors` inputs are skipped in this stage and tried in step 6 (see the warning below). Labels such as `CPU Socket Temperature`, `CPU VRM Temperature`, `PCH_CPU_TEMP` (relayed by the chipset) and `SMBUSMASTER 0` (board dependent) are never taken.
5. An input whose `temp*_type` is `6` (Intel PECI) or `5` (AMD SB-TSI), as unlabelled `it87` chips report them. The input's label, if any, is not checked. These are the CPU's own digital sensor. Ties go to the lower hwmon index, then the lower input number. Doctor shows this rule as `sensor-type`.
6. A board label on an `asus_wmi_sensors` input, using the same label table. It comes after the sensor type stage because of its polling hazard.
7. The legacy paths `thermal_zone0`, `thermal_zone1`, `hwmon0/temp1_input`, `hwmon1/temp1_input`.

Board labels, in priority order:

| Priority | Label | hwmon `name` |
|---|---|---|
| 1 | `CPU Package` | `asusec` |
| 2 | `PECI Agent 0` | `nct6775` family |
| 3 | `PECI Agent 0 Calibration` | `nct6792` to `nct6798` |
| 4 | `PECI/TSI Agent 0 Calibration` | `nct6799` |
| 5 | `TSI0_TEMP` | `nct6775` family TSI inputs (for example `nct6799` on AMD boards) |
| 6 | `AMD SB-TSI` | `nct6775` |
| 7 | `PECI 0.0` | `nct6683` (Intel boards) |
| 8 | `AMD TSI Addr 98h` | `nct6683`, `nct6686`, `nct6687` (AMD boards) |
| 9 | `CPU Temperature` | `asus_wmi_sensors`, `hp_wmi_sensors` |
| 10 | `CPU0 Temperature` | `hp_wmi_sensors` (multi-socket; socket 0 wins) |
| 11 | `CPU` | `asusec`, `dell_smm`, out-of-tree `nct6687` |
| 12 | `CPU1` | `lenovo_ec` (ThinkStation; socket 1 is the first) |
| 13 | `SoC Temperature` | `apm_xgene` (Ampere eMAG/Altra) |
| 14 | `CPUTIN` | `nct6775` family (socket thermistor, ranks last) |

`all-smi doctor --only platform.cpu_temp` shows which rule matched, the driver and label, the file, and the current reading:

```text
PASS platform.cpu_temp                board-label nct6798 "PECI Agent 0" at /sys/class/hwmon/hwmon2/temp7_input, 48°C
```

**`asus_wmi_sensors` polling.** The [kernel documentation](https://docs.kernel.org/hwmon/asus_wmi_sensors.html) warns that on some ASUS boards (PRIME X470-PRO is named) the BIOS WMI interface can stop the fans, pin them at full speed, or freeze readouts, and that frequent polling makes this more likely. all-smi reads the CPU temperature every sample, so it uses `asus_wmi_sensors` only when no other chip offers a CPU label or a PECI or SB-TSI sensor type. When it does, `all-smi doctor` reports `platform.cpu_temp` as a warning, which makes `all-smi doctor` exit with code 1 when nothing fails. If the board also has an `nct67xx` or `asusec` device, pin it with `ALL_SMI_CPU_TEMP_SENSOR`. An override that names `asus_wmi_sensors` yourself passes, with a note.

**Out-of-tree and unsupported chips.** Some boards need a driver outside the mainline kernel before any CPU sensor appears:

- MSI boards with `nct6687`: the [Fred78290 `nct6687d`](https://github.com/Fred78290/nct6687d) driver exports the CPU input as `CPU`, which is recognised.
- `it8686` and `it8688` (many Gigabyte boards) have no mainline driver; the [frankcrawford `it87`](https://github.com/frankcrawford/it87) fork supports them. These chips export no labels, so the input is found by `temp*_type` when the board reports it, and otherwise needs an override.
- `nct6683` binds to `nct6686` and `nct6687` on non-Intel boards only with the `force=1` module parameter.

If the value looks stuck or wrong (often a motherboard `thermal_zone0` reading), or the chip has no labels and no `temp*_type` hint (`it87`, `gigabyte_wmi`, `f71882fg`, where the CPU input depends on board wiring), name the sensor explicitly with `ALL_SMI_CPU_TEMP_SENSOR`:

```bash
# Input 2 of the first it87 chip that reads
ALL_SMI_CPU_TEMP_SENSOR=it87:temp2 all-smi
# Gigabyte it8688 boards (for example X570 AORUS MASTER): lm-sensors configs map
# the CPU to temp3, but this varies by board and BIOS, so verify under load
ALL_SMI_CPU_TEMP_SENSOR=it8688:temp3 all-smi
# The input labelled exactly CPUTIN on an nct6798 chip
ALL_SMI_CPU_TEMP_SENSOR=nct6798:CPUTIN all-smi
# An absolute path to a temp*_input or a thermal zone temp file under /sys
ALL_SMI_CPU_TEMP_SENSOR=/sys/class/hwmon/hwmon3/temp2_input all-smi
```

The `<driver>:...` forms search every hwmon device with that `name`, lowest index first, so they survive hwmon renumbering across reboots; prefer them over a path. Label matching is exact and case-sensitive. An empty value counts as unset. A malformed value, a path outside `/sys`, or a sensor that matches nothing readable is ignored: all-smi continues with automatic selection, and `all-smi doctor` reports the rejection as a warning, for example ``ALL_SMI_CPU_TEMP_SENSOR=it87:temp2 ignored: no hwmon device is named `it87` ``. The variable is read once at startup, so restart all-smi after changing it. An override that points at a slow sensor (for example `drivetemp` or `nvme` inputs, which issue device commands) is read on every sample, so prefer a CPU sensor.

To find the right input, compare `sensors` (lm-sensors) with the raw sysfs files:

```bash
grep . /sys/class/hwmon/hwmon*/name
grep . /sys/class/hwmon/hwmon*/temp*_label
grep . /sys/class/hwmon/hwmon*/temp*_type    # 6 = Intel PECI, 5 = AMD SB-TSI
grep . /sys/class/thermal/thermal_zone*/type
grep . /sys/class/hwmon/hwmon*/temp*_input   # millidegrees Celsius
```

Load the CPU under a stress test and pick the input that rises with it.
