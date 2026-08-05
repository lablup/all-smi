# all-smi

[![Crates.io version](https://img.shields.io/crates/v/all-smi.svg?style=flat-square)](https://crates.io/crates/all-smi)
[![Crates.io downloads](https://img.shields.io/crates/d/all-smi.svg?style=flat-square&label=crates.io%20downloads)](https://crates.io/crates/all-smi)
![GitHub Downloads](https://img.shields.io/github/downloads/lablup/all-smi/total?style=flat-square&label=GitHub%20downloads)
![CI](https://github.com/lablup/all-smi/workflows/CI/badge.svg)
[![dependency status](https://deps.rs/repo/github/lablup/all-smi/status.svg)](https://deps.rs/repo/github/lablup/all-smi)


`all-smi` is a command-line utility for monitoring GPU and NPU hardware across multiple systems. It provides a real-time view of accelerator utilization, memory usage, temperature, power consumption, and other metrics. The tool is designed to be a cross-platform alternative to `nvidia-smi`, with support for NVIDIA GPUs, AMD GPUs, NVIDIA Jetson platforms, Apple Silicon GPUs, Intel Arc/Iris Xe/Xe client GPUs, Intel Gaudi NPUs, Google Cloud TPUs, Tenstorrent NPUs, Rebellions NPUs, and Furiosa NPUs.

The application presents a terminal-based user interface with cluster overview, interactive sorting, and both local and remote monitoring capabilities. It also provides an API mode for Prometheus metrics integration.

![screenshot](screenshots/all-smi-macos.png)

<p align="center">Local-node view (on macOS)</p>

![screenshot](screenshots/all-smi-all-tab.png)

<p align="center">All-node view (remote mode)</p>

![screenshot](screenshots/all-smi-node-tab.png)

<p align="center">Node view (remote mode)</p>

## Installation

### Option 1: Install via Homebrew (macOS/Linux)

The easiest way to install all-smi on macOS and Linux is through Homebrew:

```bash
brew tap lablup/tap
brew install all-smi
```

### Option 2: Install via Ubuntu PPA

For Ubuntu users, all-smi is available through the official PPA:

```bash
# Add the PPA repository
sudo add-apt-repository ppa:lablup/backend-ai
sudo apt update

# Install all-smi
sudo apt install all-smi
```

The PPA provides automatic updates and is maintained for Ubuntu 22.04 (Jammy) and 24.04 (Noble).

### Option 3: Install via Debian Package

For Debian and other Debian-based distributions, download the `.deb` package from the [releases page](https://github.com/lablup/all-smi/releases):

```bash
# Download the latest .deb package (replace VERSION with the actual version)
wget https://github.com/lablup/all-smi/releases/download/vVERSION/all-smi_VERSION_OS_ARCH.deb
# Example: all-smi_0.7.0_ubuntu24.04.noble_amd64.deb

# Install the package
sudo dpkg -i all-smi_VERSION_OS_ARCH.deb

# If there are dependency issues, fix them with:
sudo apt-get install -f
```

### Option 4: Download Pre-built Binary

Download the latest release from the [GitHub releases page](https://github.com/lablup/all-smi/releases):

1. Go to https://github.com/lablup/all-smi/releases
2. Download the appropriate binary for your platform
3. Extract the archive and place the binary in your `$PATH`

> Release binaries are signed: macOS archives are notarized (so Gatekeeper does not block them as coming from an unidentified developer) and Windows binaries are Authenticode code-signed.

### Option 5: Install from Cargo

Install all-smi through Cargo:

```bash
cargo install all-smi
```

On Linux, you need build dependencies installed first:

```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev protobuf-compiler

# Fedora/RHEL
sudo dnf install pkg-config openssl-devel protobuf-compiler protobuf-devel
```

After installation, the binary will be available in your `$PATH` as `all-smi`.

### Option 6: Build from Source

See [Building from Source](DEVELOPERS.md#building-from-source) in the developer documentation.

## Usage

### Command Overview

> **Note:** This README tracks the `main` branch. See the
> [Changelog](#changelog) for what is in each release. To run the latest
> unreleased features, build from source
> ([Option 6](#option-6-build-from-source)).

```bash
# Show help
all-smi --help

# Local monitoring (requires sudo on macOS) - default when no command specified
all-smi
sudo all-smi local

# Remote monitoring (requires API endpoints)
all-smi view --hosts http://node1:9090 http://node2:9090
all-smi view --hostfile hosts.csv

# API mode (expose metrics server)
all-smi api --port 9090

# One-shot JSON/CSV/Prometheus dump for scripts
all-smi snapshot

# Capture a stream to disk for later replay
all-smi record --output trace.ndjson.zst --duration 1h

# Replay a captured stream in the TUI
all-smi view --replay trace.ndjson.zst

# Run API mode as a supervised background service (Linux/systemd)
sudo all-smi service install --now
all-smi service status
```

### Local Mode (Monitor Local Hardware)

The `local` mode monitors your local GPUs/NPUs with a terminal-based interface. This is the default when no command is specified.

```bash
# Monitor local GPUs (requires sudo on macOS)
all-smi              # Default to local mode
sudo all-smi local   # Explicit local mode

# With custom refresh interval
sudo all-smi local --interval 5
```

### Remote View Mode (Monitor Remote Nodes)

The `view` mode monitors multiple remote systems that are running in API mode. This mode requires specifying remote endpoints.

```bash
# Direct host specification (required)
all-smi view --hosts http://gpu-node1:9090 http://gpu-node2:9090

# Using host file (required)
all-smi view --hostfile hosts.csv --interval 2
```

**Note:** The `view` command requires either `--hosts` or `--hostfile`. For local monitoring, use `all-smi local` instead.

Host file format (CSV):
```
http://gpu-node1:9090
http://gpu-node2:9090
http://gpu-node3:9090
```

## Configuration

`all-smi` reads optional settings from a TOML config file. Every field has a compiled default, so a fresh install requires no file; operators only create one when they want persistent overrides (hostfile path, update interval, alert thresholds, `$/kWh`, etc.).

### File locations

| Platform | Canonical path | Also searched |
|----------|----------------|---------------|
| Linux    | `$XDG_CONFIG_HOME/all-smi/config.toml` (fallback `~/.config/all-smi/config.toml`) | `/etc/all-smi/config.toml` |
| macOS    | `~/Library/Application Support/all-smi/config.toml` | `~/.config/all-smi/config.toml`, then `/Library/Application Support/all-smi/config.toml` |
| Windows  | `%APPDATA%\all-smi\config.toml` | `%PROGRAMDATA%\all-smi\config.toml` |

Candidates are probed in order and the first existing file wins, so a per-user file always beats the machine-wide one. The machine-wide paths exist for daemons. On Linux a service running as a dedicated account has no home directory, so no per-user candidate ever resolves for it; on macOS a launchd LaunchDaemon runs outside any login session, so `~/Library/Application Support` resolves to root's home rather than the administrator's, and launchd has no environment-file mechanism to work around it; on Windows a service running as LocalSystem resolves `%APPDATA%` into `C:\Windows\System32\config\systemprofile\AppData\Roaming`, which no operator will ever edit (see [Running as a service](#running-as-a-service)). `all-smi config init` only ever writes the per-user path; creating `/etc/all-smi/config.toml`, `/Library/Application Support/all-smi/config.toml`, or `%PROGRAMDATA%\all-smi\config.toml` is an administrator's decision.

Pass `--config <PATH>` to any subcommand to override the discovery and force a specific file. A missing or malformed `--config` target is a hard error (exit 2); implicit discovery silently falls back to defaults when no candidate file exists. To print the active path for the current user without writing any file, run `all-smi config path` (also surfaced in `all-smi --help` under the "Configuration file" section).

### Precedence

Highest to lowest: **CLI flag > environment variable > config file > compiled default.** For example, `--port 9091` beats `ALL_SMI_API_PORT=9200` beats `[api] port = 9300` in `config.toml` beats the compiled default of `9090`. Env-var names follow the canonical pattern `ALL_SMI_<SECTION>_<KEY>` in upper-snake; legacy aliases from earlier releases (`ALL_SMI_ALERT_TEMP`, `ALL_SMI_ENERGY_PRICE`, etc.) keep working.

### Helpers

- `all-smi config init [--force]` writes a commented example config to the platform-canonical path. Refuses to overwrite without `--force`. The file is created with `O_NOFOLLOW` and mode `0o600` on Unix.
- `all-smi config print [--format toml|json] [--show-secrets]` prints the fully merged effective configuration. `webhook_url` is redacted unless `--show-secrets` is passed.
- `all-smi config validate [<path>] [--strict]` parses a config file and reports any errors (with line/column on parse failures). Exit 0 valid, 2 invalid. `--strict` rejects unknown keys.
- `all-smi config path [--json]` prints the active config-file path with an `(active)` / `(not found)` marker, plus the candidate search order. Read-only — no file is created. The same active path is shown in `all-smi --help` under the "Configuration file" block.

### Reload

Config reload is not supported in v1 — restart the process to pick up changes. This keeps the Prometheus counter and WAL state semantics simple.

### Schema

The canonical schema carries `schema_version = 1` at the top level and nine sections. Unknown keys are tolerated by default (forward compat) and reported by `config print`; `config validate --strict` rejects them. Future schema versions produce a hard error instead of silently loading.

**`[general]`** — cross-mode defaults

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `default_mode` | string | `"local"` | `ALL_SMI_GENERAL_DEFAULT_MODE` | Which subcommand runs when none is specified: `"local"`, `"view"`, or `"api"`. |
| `theme` | string | `"auto"` | `ALL_SMI_GENERAL_THEME` | TUI colour theme: `"auto"`, `"light"`, `"dark"`, `"high-contrast"`, `"mono"`. |
| `locale` | string | `"en"` | `ALL_SMI_GENERAL_LOCALE` | Display locale (reserved for future i18n). |

**`[local]`** — options for `all-smi local`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `interval_secs` | integer | adaptive | `ALL_SMI_LOCAL_INTERVAL_SECS` | Collection interval in seconds. `0` (or omit) for adaptive pacing. |

**`[view]`** — options for `all-smi view`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `hostfile` | string | — | `ALL_SMI_VIEW_HOSTFILE` | Path to a CSV/newline file listing remote `http://host:port` endpoints. |
| `hosts` | array of strings | `[]` | `ALL_SMI_VIEW_HOSTS` (comma-separated) | Inline list of remote endpoints (alternative to `hostfile`). |
| `interval_secs` | integer | adaptive | `ALL_SMI_VIEW_INTERVAL_SECS` | Scrape interval in seconds. `0` for adaptive (based on host count). |

**`[api]`** — options for `all-smi api`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `port` | integer | `9090` | `ALL_SMI_API_PORT` | TCP port for the Prometheus metrics endpoint. `0` disables TCP. |
| `socket` | bool or string | `false` | `ALL_SMI_API_SOCKET` | Unix socket: `false` = disabled, `true` = platform default path, or an explicit path string. |
| `processes` | bool | `false` | `ALL_SMI_API_PROCESSES` | Include per-process GPU metrics in `/metrics` output. |
| `interval_secs` | integer | `3` | `ALL_SMI_API_INTERVAL_SECS` | Collection interval in seconds. |

**`[alerts]`** — threshold alerting

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `enabled` | bool | `true` | — | Master switch for all alert rules. |
| `temp_warn_c` | integer | `80` | `ALL_SMI_ALERTS_TEMP_WARN_C` | GPU temperature warning threshold in °C. |
| `temp_crit_c` | integer | `90` | `ALL_SMI_ALERTS_TEMP_CRIT_C` | GPU temperature critical threshold in °C. |
| `util_idle_pct` | integer | `5` | `ALL_SMI_ALERTS_UTIL_IDLE_PCT` | Utilisation % below which a GPU is considered idle. |
| `util_idle_warn_mins` | integer | `15` | `ALL_SMI_ALERTS_UTIL_IDLE_WARN_MINS` | Minutes idle before an alert fires. |
| `hysteresis_c` | integer | `2` | `ALL_SMI_ALERTS_HYSTERESIS_C` | °C gap below a threshold before an alert clears. |
| `bell_on_critical` | bool | `false` | `ALL_SMI_ALERTS_BELL_ON_CRITICAL` | Ring the terminal bell when a critical alert triggers. |
| `webhook_url` | string | `""` | `ALL_SMI_ALERTS_WEBHOOK_URL` | HTTP(S) URL to POST JSON alert payloads to. Redacted in `config print` unless `--show-secrets`. |
| `power_crit_w` | integer | `0` | `ALL_SMI_ALERTS_POWER_CRIT_W` | GPU power draw critical threshold in watts (`0` = disabled). |

**`[energy]`** — energy accounting and cost estimation

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `price_per_kwh` | float | `0.12` | `ALL_SMI_ENERGY_PRICE_PER_KWH` | Electricity price in $/kWh for cost estimation. |
| `currency` | string | `"USD"` | `ALL_SMI_ENERGY_CURRENCY` | Currency symbol shown in the TUI. |
| `show_cost` | bool | `true` | `ALL_SMI_ENERGY_SHOW_COST` | Toggle cost column in the TUI. |
| `wal_path` | string | platform cache dir + `all-smi/energy-wal.bin` [^cache] | `ALL_SMI_ENERGY_WAL_PATH` | Path to the energy write-ahead log for persistent kWh accumulation. Operator override is resolved with `~` expansion; unset uses the platform cache helper (#229). |
| `gap_interpolate_seconds` | integer | `10` | `ALL_SMI_ENERGY_GAP_INTERPOLATE_SECONDS` | Max gap (1–3600 s) to interpolate across when the WAL has a hole. |
| `wal_enabled` | bool | `true` | `ALL_SMI_ENERGY_WAL_ENABLED` | Enable the WAL. Disable in read-only container environments. |

**`[display]`** — TUI cosmetics

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `color_scheme` | string | `"default"` | `ALL_SMI_DISPLAY_COLOR_SCHEME` | Colour palette: `"default"`, `"colorblind"`, `"mono"`. |
| `gauge_style` | string | `"blocks"` | `ALL_SMI_DISPLAY_GAUGE_STYLE` | Bar-chart fill style: `"blocks"` or `"braille"`. |
| `show_led_grid` | bool | `true` | `ALL_SMI_DISPLAY_SHOW_LED_GRID` | Show the per-GPU LED-style utilisation grid. |

**`[record]`** — defaults for `all-smi record`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `output_dir` | string | platform cache dir + `all-smi/records` [^cache] | `ALL_SMI_RECORD_OUTPUT_DIR` | Directory where recording segments are written. Operator override is resolved with `~` expansion; unset uses the platform cache helper (#229). |

[^cache]: The platform cache directory is `$XDG_CACHE_HOME` (or `~/.cache`) on Linux, `~/Library/Caches` on macOS, and `%LOCALAPPDATA%` on Windows. All three cache consumers (record output, energy WAL, users-CSV export) go through `dirs::cache_dir()` so the layout is consistent across platforms.
| `compress` | string | `"zstd"` | `ALL_SMI_RECORD_COMPRESS` | Compression codec: `"zstd"`, `"gzip"`, or `"none"`. |

**`[snapshot]`** — defaults for `all-smi snapshot`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `default_format` | string | `"json"` | `ALL_SMI_SNAPSHOT_DEFAULT_FORMAT` | Output format: `"json"`, `"csv"`, or `"prometheus"`. |
| `default_pretty` | bool | `true` | `ALL_SMI_SNAPSHOT_DEFAULT_PRETTY` | Pretty-print JSON output. |

### Legacy environment variable aliases

Older releases introduced environment variables with different naming. All aliases remain supported; when both the legacy and canonical name are set, the canonical name takes precedence.

| Legacy alias | Canonical equivalent | Description |
|---|---|---|
| `ALL_SMI_ALERT_TEMP` | `ALL_SMI_ALERTS_TEMP_WARN_C` | Temperature warning threshold (°C); also auto-raises `temp_crit_c` if needed. |
| `ALL_SMI_ALERT_UTIL_LOW_MINS` | `ALL_SMI_ALERTS_UTIL_IDLE_WARN_MINS` | Minutes idle before alert fires. |
| `ALL_SMI_ENERGY_PRICE` | `ALL_SMI_ENERGY_PRICE_PER_KWH` | Price per kWh. |
| `ALL_SMI_ENERGY_NO_COST` | `ALL_SMI_ENERGY_SHOW_COST=false` | Set to `1`/`true` to hide the cost column. |
| `ALL_SMI_ENERGY_WAL_PATH` | `ALL_SMI_ENERGY_WAL_PATH` | WAL file path (same canonical name). |
| `ALL_SMI_ENERGY_NO_WAL` | `ALL_SMI_ENERGY_WAL_ENABLED=false` | Set to `1`/`true` to disable WAL. |
| `ALL_SMI_ENERGY_GAP_SECONDS` | `ALL_SMI_ENERGY_GAP_INTERPOLATE_SECONDS` | Interpolation gap in seconds. |
| `ALL_SMI_ENERGY_CURRENCY` | `ALL_SMI_ENERGY_CURRENCY` | Currency symbol (same canonical name). |

## Running as a service

`all-smi api` is the data source behind `all-smi view --hosts/--hostfile`, so on a cluster it wants to start at boot, restart on failure, and log to the platform's journal. The `all-smi service` subcommand installs and controls it:

```bash
all-smi service install   [--user] [--service-user NAME] [--now] [--force]
all-smi service uninstall [--user] [--force]
all-smi service start     [--user]
all-smi service stop      [--user]
all-smi service restart   [--user]
all-smi service status    [--user] [--json]
```

The default scope is system-wide and requires root; `--user` installs a per-user service with no elevation. `status` exits `0` when the service is running and `3` when it is stopped or not installed, mirroring `systemctl is-active`; every other failure exits `1`.

There are deliberately no `--port` or `--interval` flags on `install`. Runtime configuration lives in the environment file and the TOML config file, so changing a setting never means regenerating and reinstalling the service definition.

<!-- Additional platform subsections (macOS launchd, Windows Service Control Manager) are appended below the Linux subsection as they land. -->

### Linux (systemd)

The canonical unit is [`packaging/systemd/all-smi.service`](packaging/systemd/all-smi.service). Both installation paths use it: the Debian package ships it verbatim, and `all-smi service install` embeds the same file and rewrites `ExecStart=` plus the account directives for the scope you asked for.

**From the Debian package or PPA.** The package installs the unit but leaves it disabled, because opening a listening port as a side effect of `apt install` would be a surprise on a machine where `all-smi` is just a CLI tool:

```bash
sudo apt install all-smi
sudo systemctl enable --now all-smi
curl -s localhost:9090/metrics | head
journalctl -u all-smi -f
```

The package also creates a dedicated `all-smi` system account and installs `/etc/default/all-smi` as a conffile for environment overrides.

**From a tarball, `cargo install`, or a local build.** Use the subcommand:

```bash
# System-wide, started immediately.
sudo all-smi service install --now
all-smi service status

# Run as a dedicated account instead of root (recommended where your
# vendor CLI permits it; create the account first).
sudo all-smi service install --service-user all-smi --now

# Per-user, no root. Add `loginctl enable-linger $USER` for boot persistence.
all-smi service install --user --now
all-smi service status --user

sudo all-smi service uninstall
```

The subcommand default is root, not a dedicated account, because vendor CLIs (`hl-smi`, `rbln-stat`, `furiosa-smi`, `tegrastats`) differ in what permissions they need, and a wrong guess yields a silently empty metrics page. The Debian package can take the opposite default because its unit is tested against the `all-smi` account.

`install` refuses to run when a package manager already owns the binary (`dpkg` at `/usr/bin/all-smi`, or a Homebrew prefix) and points at that package manager's own command instead. Both `install` and `uninstall` also refuse to touch a unit file that lacks the `# Managed by 'all-smi service'` marker. Pass `--force` to override either refusal.

**Configuration.** The system service reads `/etc/all-smi/config.toml`; a user service reads your usual per-user config path. Environment variables from `/etc/default/all-smi` take precedence over the TOML file:

```sh
# /etc/default/all-smi
ALL_SMI_API_PORT=9090
ALL_SMI_API_INTERVAL_SECS=3
RUST_LOG=info
```

`/etc/all-smi/config.toml` exists as its own discovery candidate because a service running as a dedicated account has no home directory, and because the unit sets `ProtectHome=true`, which makes even root's `~/.config` invisible to the service. `all-smi config path` lists the full search order for the current user.

**Unit hardening.** The unit enables `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `ProtectKernelModules`, `ProtectControlGroups`, and `RestrictSUIDSGID`. Two directives are deliberately absent and should stay that way: `PrivateDevices=` would hide `/dev/nvidia*` and `/dev/dri/*` from NVML and the AMD/Intel readers, and `ProtectProc=`/`ProcSubset=` would hide the processes the process-metrics reader enumerates. `SupplementaryGroups=video render` covers the DRM render nodes under stock Debian and Ubuntu udev rules.

A user-scope unit keeps only the hardening that needs no privilege, `NoNewPrivileges=` and `RestrictSUIDSGID=`, and drops `SupplementaryGroups=`, `ProtectKernelModules=`, `ProtectSystem=`, `ProtectHome=`, `PrivateTmp=`, and `ProtectControlGroups=`. A `systemd --user` manager runs unprivileged, and a directive whose setup it cannot perform makes the unit fail *before* `ExecStart`, so the service does not start at all: `SupplementaryGroups=` exits `216/GROUP`, and the rest need a user namespace, which Ubuntu 24.04 and later restrict through AppArmor, giving `218/CAPABILITIES` or `226/NAMESPACE`.

`ProtectKernelModules=` is the surprising member of that set. It reads as pure seccomp, but it also strips `CAP_SYS_MODULE` from the capability bounding set, which an unprivileged manager can only do from inside a user namespace. Dropping it costs nothing: a user service never holds `CAP_SYS_MODULE` to begin with, so module loading is already impossible. `ProtectHome=` is dropped for a second reason too, since it would hide your own `~/.config/all-smi/config.toml` from your own service.

**Unix socket.** `PrivateTmp=true` namespaces `/tmp`, so the default `/tmp/all-smi.sock` fallback would not be reachable from outside the service. The unit provides `/run/all-smi` through `RuntimeDirectory=`; set `ALL_SMI_API_SOCKET=/run/all-smi/all-smi.sock` in the environment file to expose the socket there.

**Other init systems.** OpenRC, runit, and sysvinit are not supported by the subcommand; it detects the absence of systemd and points at the canonical unit for manual adaptation.

### macOS (launchd)

The canonical job definition is [`packaging/launchd/com.lablup.all-smi.plist`](packaging/launchd/com.lablup.all-smi.plist), a self-contained system LaunchDaemon you can also copy into `/Library/LaunchDaemons` by hand. `all-smi service install` embeds the same file and rewrites `ProgramArguments`, the log paths, and the account keys for the scope you asked for. The launchd label is `com.lablup.all-smi` in both scopes; they live in different domains, so the name never collides.

**From Homebrew.** The formula ships a service block, so `brew services` is the supported path and the subcommand refuses to install alongside it:

```bash
brew install lablup/tap/all-smi

# Per-user. Bootstraps into gui/$UID, so it stops at logout.
brew services start all-smi

# Boot-time. Bootstraps into the system domain and survives a reboot
# with nobody logged in. This is the one a headless node wants.
sudo brew services start all-smi

curl -s localhost:9090/metrics | head
tail -f "$(brew --prefix)/var/log/all-smi.log"
```

The two invocations are not interchangeable. Without `sudo`, `brew services` targets your GUI login session; a rack-mounted Mac mini or Studio that reboots unattended will come back with no exporter running. With `sudo` it targets the system domain and starts at boot.

**From a zip, `cargo install`, or a local build.** Use the subcommand:

```bash
# System-wide LaunchDaemon at /Library/LaunchDaemons, started immediately.
sudo all-smi service install --now
all-smi service status
sudo all-smi service uninstall

# Per-user LaunchAgent at ~/Library/LaunchAgents, no root.
all-smi service install --user --now
all-smi service status --user
```

Logs go to `/var/log/all-smi/all-smi.log` for the daemon and `~/Library/Logs/all-smi/all-smi.log` for the agent, through `StandardOutPath` and `StandardErrorPath`. `uninstall` boots the job out and removes the plist but leaves the log behind, so you can still read why you removed it.

**launchd has no separate "enabled" state.** A plist sitting in `LaunchDaemons` or `LaunchAgents` is bootstrapped automatically at boot or login, and `RunAtLoad` starts it from there. So `install` without `--now` writes the plist and stops, which is precisely "enabled at boot, not running yet"; `install --now` additionally boots the job out and back in, because launchd caches a loaded job's definition and bootstrapping over it fails rather than replacing it. `stop` boots the job out of its domain and leaves the plist, so the service returns at the next boot, matching `systemctl stop`. `install` also runs `launchctl enable` to clear a persistent disable override, which otherwise outlives both the plist and a reboot; `uninstall` deliberately does not `disable`, for the same reason.

**Configuration.** launchd has no `EnvironmentFile=` equivalent, so unlike the Linux service there is no `/etc/default/all-smi` analogue: the TOML config is the whole story. A system LaunchDaemon runs outside any login session, so `~/Library/Application Support` resolves to root's home rather than yours. `/Library/Application Support/all-smi/config.toml` is a discovery candidate exactly for that case, ordered after every per-user candidate so your own file still wins when both exist. `all-smi config path` lists the full search order.

**Which plist keys a LaunchAgent gets.** A user-scope render drops `UserName`, `GroupName`, and `InitGroups`, all of which need root. On macOS 26.6 launchd does not reject them the way systemd rejects an unprivileged user unit: bootstrapping a LaunchAgent that carries `UserName root` succeeds and the job still runs as you, with the key silently ignored. They are dropped because keeping them would ship a plist that reads, to anyone auditing what runs privileged on the machine, as a root job when it is not, and because Apple documents the keys as requiring root without documenting the fallback. Everything else is kept in both scopes, including `SoftResourceLimits`, since raising a soft rlimit up to the inherited hard limit needs no privilege.

`--service-user` sets `UserName` in system scope and drops `GroupName` rather than mirroring the account name the way the Linux renderer does. macOS has no convention that an account owns an eponymous group, so omitting the key makes launchd use the account's primary group straight from the password database. Give such an account a writable home directory, or point `[energy] wal_path` somewhere it can write, or the energy WAL degrades to in-memory with a warning in the log.

**Apple Silicon metrics under launchd.** The native readers (IOReport, the SMC, and `NSProcessInfo.thermalState`) need no GUI session and no sudo, and a LaunchAgent exports the same metric set a foreground `all-smi api` does: GPU utilization and power, SMC GPU and CPU temperatures, ANE power, thermal pressure, and P/E cluster frequencies. The plist sets `ProcessType` to `Background` so the exporter runs at background QoS on the efficiency cores and does not compete with the workload it is watching. That costs a few seconds of extra startup, because enumerating the IOReport channel list is the expensive part of coming up; it is a one-time cost per launch.

### Windows (Service Control Manager)

`all-smi service` registers a native Windows service through the Service Control Manager. Nothing is shelled out to `sc.exe`, and the release zip needs no extra files: the same `all-smi.exe` is both the CLI and the service host.

Every mutating action needs Administrator rights. Run them from an elevated Command Prompt, Windows Terminal, or PowerShell (right-click, "Run as administrator"); an unelevated attempt exits `1` with an explanation rather than a bare `os error 5`. `all-smi service status` works without elevation.

```powershell
# Register and start. Starts automatically at boot from then on,
# with no logged-in user.
all-smi service install --now
all-smi service status
Invoke-WebRequest http://localhost:9090/metrics -UseBasicParsing | Select-Object -ExpandProperty Content

all-smi service restart
all-smi service stop
all-smi service uninstall
```

The service is named `all-smi` and appears in `services.msc` as **all-smi GPU/NPU Metrics Exporter**. It runs as **LocalSystem**, because NVML, the WMI thermal-zone classes under `root\cimv2` and `root\wmi`, the AMD Ryzen Master interface, and LibreHardwareMonitor-style sensor access all need it. Start type is plain automatic rather than delayed, so metrics exist early in boot. If the process dies, the SCM restarts it after 5 seconds, up to three times before the failure counter resets a day later.

`--user` is **not supported on Windows**: the SCM has no per-user service scope, so this is a platform limit rather than a missing feature. For a non-admin, per-login exporter, register a Task Scheduler task instead:

```powershell
schtasks /create /tn all-smi /tr "C:\path\to\all-smi.exe api" /sc onlogon
```

`all-smi service run` exists but is hidden: it is the SCM entry point, not a way to start the exporter by hand. Run from a console it explains itself and exits `1`. Use `all-smi api` for a foreground server.

**Idempotency.** Re-running `install` over a service that already points at the same `all-smi.exe` updates its configuration in place. A service of the same name that points somewhere else is refused, with the two paths named; pass `--force` to repoint it anyway. `uninstall` applies the same guard. This is the Windows counterpart of the Linux managed-by marker: the SCM offers nowhere to stamp one, so the registered binary path is the identity check.

Reconfiguring an already-running service does not restart it, matching `systemctl enable --now`. Run `all-smi service restart` to pick up a new binary path or config file.

**Configuration.** The service reads `%PROGRAMDATA%\all-smi\config.toml` (usually `C:\ProgramData\all-smi\config.toml`). That path exists because a service running as LocalSystem resolves `%APPDATA%` into `C:\Windows\System32\config\systemprofile\AppData\Roaming`, which no operator will ever edit. Your own `%APPDATA%\all-smi\config.toml` still wins for interactive runs; `all-smi config path` lists the full search order.

```toml
# C:\ProgramData\all-smi\config.toml
[api]
port = 9090
interval_secs = 3
```

Restart the service after editing it. Environment variables such as `RUST_LOG` are set for the service through its registry key, in the `Environment` value (type `REG_MULTI_SZ`) under `HKLM\SYSTEM\CurrentControlSet\Services\all-smi`.

**Logs.** stdout is void under the SCM, so the service writes to `%PROGRAMDATA%\all-smi\logs\all-smi.<date>.log`, rotated daily and pruned to the last 14 files. The default level is `info`; raise it with `RUST_LOG` through the registry value above.

**Firewall.** `all-smi` never touches the firewall. To let other hosts scrape the exporter, open the port yourself from an elevated prompt:

```powershell
netsh advfirewall firewall add rule name="all-smi" dir=in action=allow protocol=TCP localport=9090
netsh advfirewall firewall delete rule name="all-smi"
```

## Platform-Specific Requirements

### macOS (Apple Silicon)
- **No sudo required:** Uses native macOS APIs for metrics collection
  - Uses IOReport API and Apple SMC directly
  - Provides actual temperature readings from SMC sensors
  - Run with: `all-smi local`

### macOS (Intel)
- **No sudo required:** Uses the Apple SMC and NSProcessInfo directly
  - CPU model, socket/core/thread counts, and clocks come from `sysctl`
  - Per-core utilization bars, plus CPU temperature from the SMC `TC0P`/`TC0D` sensors
  - Chassis block reports fan RPMs, thermal pressure, and approximate total system power (SMC `PSTR`)
  - GPU monitoring is not available: the integrated Intel and discrete AMD GPUs in Intel Macs are not read, so the GPU list stays empty
  - Total power is the SMC's own estimate rather than a metered value, and its accuracy varies by model. `powermetrics` would be metered but needs sudo, which all-smi does not ask for on macOS
  - Download `all-smi-macos-x86_64.zip` from the releases page. It is signed and notarized like the Apple Silicon archive
  - Run with: `all-smi local`

### Linux with AMD GPUs
- **Sudo Access Required:** AMD GPU monitoring requires `sudo` to access `/dev/dri` devices
- **ROCm Installation:** AMD GPU support requires ROCm drivers and libraries
- **Build Requirements:**
  - AMD GPU support is available in **glibc builds only** (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`)
  - **Not available in musl builds** (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) due to library compatibility
  - For static binaries with AMD GPU support, use the glibc builds
- **Permissions:** Add user to `video` and `render` groups as an alternative to sudo:
  ```bash
  sudo usermod -a -G video,render $USER
  # Log out and back in for changes to take effect
  ```

### Linux with NVIDIA GPUs
- **No Sudo Required:** NVIDIA GPU monitoring works without sudo privileges
- **Driver Required:** NVIDIA proprietary drivers must be installed

### Linux with Intel GPUs
- **No Sudo Required (baseline):** Intel Arc / Iris Xe / Xe client GPU monitoring reads `i915`/`xe` sysfs and `hwmon`, which works without elevated privileges
- **Driver Required:** A kernel with the `i915` (integrated Iris Xe / Xe-LPG and earlier discrete Arc) or `xe` (newer discrete Arc) driver loaded
- **Per-Process Memory (`--processes`):** Attribution reads `/proc/<pid>/fdinfo`; entries for processes owned by other users may be unavailable and degrade silently per-process (run with `sudo` to attribute every process)
- **Optional Level Zero Metrics:** Build with `--features level_zero` and install the Intel oneAPI Level Zero runtime so `libze_loader.so.1` is present at runtime. When available, Sysman adds per-engine activity (including the XMX `COMPUTE_SINGLE` class), energy-counter power, temperature, memory, and frequency on top of the sysfs baseline; when absent, all-smi silently falls back to the sysfs baseline

### Windows
- **No Sudo Required:** GPU and CPU monitoring works without administrator privileges
- **Intel client GPUs:** Arc / Iris Xe / Xe metrics are collected via WMI; building with `--features level_zero` adds Intel Level Zero (Sysman) metrics when `ze_loader.dll` (Intel GPU driver / oneAPI runtime) is present, otherwise the WMI baseline is used
- **CPU Temperature Limitations:**
  - Standard Windows WMI thermal zones (MSAcpi_ThermalZoneTemperature) are not available on all systems
  - The application uses a fallback chain to try multiple temperature sources:
    1. ACPI Thermal Zones (standard WMI)
    2. AMD Ryzen Master SDK (AMD CPUs - requires AMD drivers or Ryzen Master)
    3. Intel WMI (Intel CPUs - if chipset drivers support it)
    4. LibreHardwareMonitor WMI (any CPU - if [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor) is running)
  - If temperature is not available, it will be shown as "N/A" without error messages
  - For best temperature monitoring on Windows, install and run LibreHardwareMonitor in the background

## Diagnostics

The `all-smi doctor` subcommand runs a read-only suite of environment checks and
prints a PASS/WARN/FAIL report covering platform, privileges, container
runtime, every supported hardware backend (NVIDIA, AMD, Apple, Gaudi, TPU,
Tenstorrent, Rebellions, Furiosa, Windows), the relevant environment
variables, and optional remote endpoint connectivity. Each check has a hard
3-second timeout.

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
| `platform.*` | `platform.os`, `platform.runtime`, `platform.cpu`, `platform.memory`, `platform.hardware`, `platform.uptime` |
| `privileges.*` | `privileges.user`, `privileges.root`, `privileges.video_render_group`, `privileges.dev_dri`, `privileges.dev_tenstorrent` |
| `container.*` | `container.runtime`, `container.cgroup`, `container.k8s_serviceaccount` |
| `nvidia.*` | `nvidia.nvml.loadable`, `nvidia.smi.binary`, `nvidia.driver.version`, `nvidia.env.visible_devices`, `nvidia.mig.mode` |
| `amd.*` | `amd.rocm.version`, `amd.libamdgpu_top.abi`, `amd.dri.perms`, `amd.build.target_env` |
| `apple.*` | `apple.macos.version`, `apple.silicon`, `apple.smc` |
| `gaudi.*` | `gaudi.hlsmi`, `gaudi.devices`, `gaudi.driver` |
| `tpu.*` | `tpu.libtpu`, `tpu.env.name`, `tpu.accel.vendor` |
| `tenstorrent.*` | `tenstorrent.luwen`, `tenstorrent.kmd`, `tenstorrent.module` |
| `rebellions.*` | `rebellions.rblnstat`, `rebellions.driver` |
| `furiosa.*` | `furiosa.feature`, `furiosa.smi` |
| `windows.*` | `windows.wmi`, `windows.amd_ryzen_master`, `windows.intel_wmi`, `windows.libre_hardware_monitor` |
| `env.*` | `env.all_smi`, `env.cuda`, `env.rocr`, `env.tpu`, `env.hl` |
| `network.*` | `network.dns`, `network.tcp`, `network.http` |

## Features

### GPU Monitoring
- **Real-time Metrics:** Displays comprehensive GPU information including:
  - GPU Name and Driver Version
  - Utilization Percentage with color-coded status
  - Memory Usage (Used/Total in GB)
  - Temperature in Celsius (or Thermal Pressure for Apple Silicon)
  - Clock Frequency in MHz
  - Power Consumption in Watts (2 decimal precision for Apple Silicon)
- **Multi-GPU Support:** Handles multiple GPUs per system with individual monitoring
- **Interactive Sorting:** Sort GPUs by utilization, memory usage, or default (hostname+index) order
- **Platform-Specific Features:**
  - NVIDIA: PCIe info, performance states (P0–P15), thermal thresholds (slowdown/shutdown/max-operating/acoustic), power limits, vGPU SR-IOV monitoring, MIG (Multi-Instance GPU) monitoring with per-instance utilization and memory metrics, hardware details (NUMA node, GSP firmware mode and version, NvLink remote endpoint classification, GPM SM occupancy and memory bandwidth utilization)
  - AMD: VRAM/GTT memory tracking, fan speed monitoring, GPU process detection with fdinfo
  - NVIDIA Jetson: DLA utilization monitoring
  - Apple Silicon: ANE power monitoring, thermal pressure levels
  - Intel Gaudi NPUs: AIP utilization monitoring, HBM memory tracking, device variant detection (PCIe/OAM/UBB)
  - Google Cloud TPUs: Support for TPU v2-v7/Ironwood, HBM memory tracking, libtpu/JAX integration
  - Tenstorrent NPUs: Real-time telemetry via luwen library, board-specific TDP calculations
  - Rebellions NPUs: Performance state monitoring, KMD version tracking, device status
  - Furiosa NPUs: Per-core PE utilization, power governor modes, firmware version tracking
  
### CPU Monitoring
- **Comprehensive CPU Metrics:**
  - Real-time CPU utilization with per-socket breakdown
  - Core and thread counts
  - Frequency monitoring (P+E format for Apple Silicon)
  - Temperature and power consumption
- **Apple Silicon Enhanced:**
  - P-core and E-core utilization tracking
  - P-cluster and E-cluster frequency monitoring
  - Integrated GPU core count

### Memory Monitoring
- **System Memory Tracking:**
  - Total, used, available, and free memory
  - Memory utilization percentage
  - Linux: Buffer and cache memory tracking
- **Swap Space Monitoring:**
  - A dedicated `Swap` bar is rendered directly under the `Mem` bar in the
    TUI memory section whenever the host has swap configured
    (`swap_total_bytes > 0`)
  - Hosts without swap (`swap_total_bytes == 0`) automatically hide the
    swap row so the layout stays compact — most relevant on Apple Silicon
    before macOS' `dynamic_pager` allocates a swap file
  - The swap bar segment is colored **red** when `swap_used_bytes > 0`
    to flag active swapping at a glance — the primary signal needed for
    AI inference workloads on Apple Silicon, where over-sized models
    spill unified memory into swap and silently degrade throughput
- **Visual Indicators:** Color-coded memory usage bars

### Process Monitoring
- **Enhanced GPU Process View:**
  - Process ID (PID) and Parent PID
  - Process Name and Command Line
  - GPU Memory Usage with per-column coloring
  - CPU usage percentage
  - User and State Information
- **Advanced Features:**
  - Mouse click sorting on column headers
  - Multi-criteria sorting (PID, memory, GPU memory, CPU usage)
  - Per-column color coding for better visibility
  - Full process tree integration

### Chassis/Node-Level Monitoring
- **System-Wide Power Tracking:**
  - Total chassis power consumption (CPU+GPU+ANE combined)
  - Individual power component breakdown
  - Real-time power efficiency monitoring
- **Thermal Monitoring:**
  - Thermal pressure levels (Apple Silicon)
  - Inlet/outlet temperature tracking (BMC-enabled servers)
  - Fan speed monitoring with per-fan granularity
- **Platform-Specific Features:**
  - Apple Silicon: CPU, GPU, ANE power breakdown with thermal pressure
  - Server systems: BMC sensor integration for comprehensive thermal monitoring

### Energy & Cost Accounting
- **Energy session row** under each chassis card shows cumulative Joules /
  kWh and, when a price is configured, the approximate monetary cost for the
  current session. The session starts when the process boots (or after the
  `R` hotkey; see below) and is reset without touching the lifetime
  Prometheus counter.
- **`R` hotkey** zeroes the per-session counter and the "session started"
  timestamp across every device so you can quickly bracket a workload.
  The Prometheus `all_smi_energy_consumed_joules_total` counter keeps
  climbing monotonically — `rate()` / `increase()` stay well-defined.
- **WAL persistence** — in API mode the integrator periodically flushes
  accumulated Joules to a write-ahead log so the Prometheus counter
  survives restarts. The file lives under the platform cache directory
  by default (`<cache>/all-smi/energy-wal.bin` — see the `[energy]`
  table above for the per-platform path), is opened with `O_NOFOLLOW` +
  mode `0o600` on Unix, and is compacted automatically once it grows
  past ~16 MiB. Flush + fsync run on a dedicated blocking task so a
  slow filesystem (NFS, SAN failover) cannot stall the tokio runtime.
- **Environment overrides** (the Prometheus counter is always exported;
  these only affect the TUI display and the WAL):
  - `ALL_SMI_ENERGY_PRICE=<price_per_kwh>` — set the price used for the
    cost column. Invalid / non-finite values are silently ignored.
  - `ALL_SMI_ENERGY_CURRENCY=<code>` — display-only currency code
    (`USD`, `KRW`, `EUR`, …).
  - `ALL_SMI_ENERGY_NO_COST=1` — hide the cost column even if a price
    is set.
  - `ALL_SMI_ENERGY_WAL_PATH=/alt/path/energy-wal.bin` — override the
    default WAL location.
  - `ALL_SMI_ENERGY_NO_WAL=1` — disable the WAL entirely (counters
    stay in memory only).
  - `ALL_SMI_ENERGY_GAP_SECONDS=<seconds>` — gap threshold above which
    the integrator holds the last sample instead of interpolating.
    Accepted range is `1..=3600`; values outside that window fall
    back to the 10-second default.

### Cluster Management

> Note: The Cluster Overview Dashboard, Live Statistics History, and Tabbed Interface appear only in remote mode (when `--hosts` or `--hostfile` is specified). Local mode replaces these with a compact two-line host summary bar showing hostname, CPU model, architecture, uptime, and live sparkline metrics (CPU%, GPU%, RAM, power, temperature).

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

### Cross-Platform Support
- **Linux:**
  - NVIDIA GPUs via NVML and nvidia-smi (fallback)
  - AMD GPUs (Radeon and Instinct) via ROCm and libamdgpu_top library
    - Real-time VRAM and GTT memory monitoring
    - GPU process detection with memory usage tracking
    - Temperature, power consumption, frequency, and fan speed metrics
    - Requires sudo access to /dev/dri devices (glibc builds only)
  - Intel Arc / Iris Xe / Xe client GPUs (Arc A/B-series, Core Ultra iGPU, Iris Xe) via i915/xe sysfs
    - Architecture classification (Alchemist, Battlemage, Xe-LPG, Iris Xe) with SYCL/oneAPI capability flag
    - Discrete VRAM tracking (i915 `mem_info_vram_total` and xe `tile0/vram0/total_bytes`)
    - Frequency, temperature (hwmon), power (hwmon), and fan (hwmon) metrics
    - Engine-busy utilization from sysfs per-engine monotonic counters (i915 and xe layouts); `max(render, compute)` reported as primary utilization; first refresh is a seeding call (returns `0.0`), real values available from the second refresh; PMU fallback for older kernels is deferred
    - Per-process GPU memory tracking via `/proc/<pid>/fdinfo` (Linux, with `--processes` flag); dedupes shared DRM file descriptors by `drm-client-id`; permission errors degrade silently per-process
    - Opt-in Level Zero Sysman backend behind `--features level_zero` (default off): dynamically loads `libze_loader.so.1` / `libze_loader.so` on Linux or `ze_loader.dll` on Windows and uses Sysman as the preferred Intel vendor source when values are fresh. Temperature, fresh energy-counter power deltas, dedicated/local memory state, fresh engine-activity deltas, and frequency can override the sysfs/WMI baseline per field; Linux fan keeps hwmon priority and falls back to Sysman, while Windows uses Sysman fan data when available. Seeded delta samples never overwrite a valid fallback, missing loaders/symbols degrade silently, and `detail["Source: <field>"]` exposes mixed-source results.
  - CPU monitoring via /proc filesystem
  - Memory monitoring with detailed statistics
  - Intel Gaudi NPUs (Gaudi 1/2/3) via hl-smi with background process monitoring
  - Google Cloud TPUs (v2-v7/Ironwood) via libtpu with JAX/Python integration
  - Tenstorrent NPUs (Wormhole, Blackhole) via luwen library
  - Rebellions NPUs (ATOM, ATOM+, ATOM Max) via rbln-stat
  - Furiosa NPUs (RNGD) via furiosa-smi
- **macOS:**
  - Apple Silicon (M1/M2/M3/M4) GPUs monitoring
  - Native APIs: IOReport, SMC for no-sudo operation
  - ANE (Apple Neural Engine) power tracking
  - Actual CPU/GPU temperature readings from SMC sensors
  - Thermal pressure monitoring
  - P/E core architecture support
  - Intel Macs (x86_64): CPU, memory, and chassis monitoring via SMC and sysctl, including per-core bars, CPU temperature, fan RPMs, and approximate system power. No GPU monitoring
- **NVIDIA Jetson:** 
  - Special support for Tegra-based systems
  - DLA (Deep Learning Accelerator) monitoring

### Remote Monitoring
- **Multi-Host Support:** Monitor up to 256+ remote systems simultaneously
- **Connection Management:** Optimized networking with:
  - Connection pooling (200 idle connections per host)
  - Concurrent connection limiting (64 max)
  - Automatic retry with exponential backoff
  - TCP keepalive for persistent connections
  - Connection staggering to prevent overload
- **Storage Monitoring:** Disk usage information for all hosts
- **High Availability:** Resilient to connection failures with automatic recovery

### Agentless SSH mode

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
# Monitor two DGX boxes over SSH, using your agent key.
all-smi view --ssh admin@dgx-01,admin@dgx-02

# Bulk mode from a hostfile (see examples/hosts-ssh.txt).
all-smi view --ssh-hostfile examples/hosts-ssh.txt

# Accept unknown host keys on first connect (TOFU) and persist them.
all-smi view --ssh admin@new-node --ssh-strict-host-key accept-new
```

Key flags:

| Flag | Default | Notes |
| --- | --- | --- |
| `--ssh user@host[:port][,...]` | — | Comma-separated SSH targets. |
| `--ssh-hostfile <path>` | — | One `user@host[:port]` per line; `#` comments allowed. |
| `--ssh-key <path>` | auto-probe | Overrides agent / `~/.ssh/id_*` probe order. |
| `--ssh-strict-host-key yes\|accept-new\|no` | `yes` | Matches OpenSSH semantics. |
| `--ssh-timeout-secs <n>` | `10` | Per-target TCP/handshake timeout. |
| `--ssh-fallback nvidia-smi,rocm-smi,none` | both enabled | Which shim(s) to try when `all-smi` is absent. |
| `--ssh-known-hosts <path>` | `~/.ssh/known_hosts` | Custom known-hosts file. |
| `--ssh-concurrency <n>` | `32` | Bound on concurrent SSH connects (semaphore-limited). |

Security notes:

- Password auth is **never** attempted; key / agent auth only. No
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

### Interactive UI
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
  - Color-coded status: Green (≤60%), Yellow (60-80%), Red (>80%)
  - Per-column coloring in process view
  - Responsive layout adapting to terminal size
  - Double-buffered rendering for flicker-free display
- **Help System:** Context-sensitive help with all keyboard shortcuts

### Cluster-Wide Users Tab (`V`)

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
directly; we proxy it with the user's share of VRAM on each GPU. The value
is clamped to ≥ 0 to guard against race conditions where the sum of process
VRAM exceeds the GPU's reported `memory_used`. The `*` in the header marks
the column as approximate.

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
  — see the `[energy]` / `[record]` table footnote for the per-platform
  path)

**Partial coverage.** When some hosts report `--processes` data and others
don't, the tab shows a yellow chip `⚠ partial coverage: M of N nodes reporting
process data` so operators don't misread the numbers. If zero hosts report
process data, the tab renders a hint pointing at the `--processes` flag
instead of an empty table.

Mock clusters can exercise the tab without real hosts by setting
`ALL_SMI_MOCK_PROCESSES=1`, which makes every synthetic node emit a small
rotation of users and commands through the `all_smi_process_*` metric
families.

### Topology View (`T`)

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

Mock clusters can exercise the tab by setting `ALL_SMI_MOCK_TOPOLOGY=1`.
Every synthetic NVIDIA node then emits a DGX-like 8-GPU topology: two
NUMA zones, full 7-link GPU-to-GPU mesh, and one switch link per GPU.

### Filtering & Alerts

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

Alert thresholds live in the compiled defaults today (the config-file loader
lands in a separate issue). Override them with CLI flags:

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

**Security notes**

- **Webhook SSRF protection**: HTTP redirects are disabled on the webhook
  client. If the configured URL responds with a 3xx redirect the response
  is logged and the redirect target is *not* followed. This prevents a
  misconfigured or attacker-controlled endpoint from pivoting the client to
  cloud-metadata services (e.g. `169.254.169.254`) or other internal-only
  hosts. Operators should configure the exact destination URL directly.
- **Filter query buffer cap**: The interactive filter bar accepts at most
  512 characters. Input beyond that limit is silently discarded. This keeps
  a bracketed-paste of large text from turning every keystroke into an
  unbounded O(n) re-parse. The lexer independently rejects inputs exceeding
  16 KiB as an additional safeguard for any programmatic caller.
- **Users tab CSV export (`e`) — symlink refusal**: The export file is
  opened with `O_NOFOLLOW` and mode `0600`. On a shared machine a co-tenant
  could pre-plant the platform cache directory or the final filename as a
  symlink to redirect the write to an arbitrary path. This is refused:
  any pre-existing symlink at either the cache directory path or the
  timestamped filename causes the export to fail with an error rather
  than follow the link. Windows falls back to `share_mode(0)` (exclusive
  access).
- **Users tab CSV export — formula injection mitigation**: Usernames and
  command lines in the exported CSV come from untrusted remote hosts.
  Spreadsheet applications (Excel, LibreOffice Calc, Google Sheets) treat
  cells whose first character is `=`, `+`, `-`, `@`, TAB, or CR as formula
  expressions and may execute embedded commands when the file is opened.
  All such fields are quoted per RFC 4180 and prefixed with a single quote
  (`'`) inside the quoted value so the spreadsheet treats them as plain text.
- **Process label caps in the remote parser**: The Prometheus scrape parser
  enforces per-field length limits on process-metric labels received from
  remote hosts — `user` and `command` are capped at 128 and 256 bytes
  respectively (truncated at a UTF-8 boundary), and the global label parser
  rejects any string longer than 1024 bytes per key or value and any metric
  block with more than 100 labels. This bounds the memory a malicious host
  can consume via the `all_smi_process_*` metric families.

### Development & Testing
- **Mock Server:** Built-in mock server for testing and development
  - Simulates realistic GPU clusters with 8 GPUs per node
  - Configurable port ranges for multiple instances
  - Failure simulation for resilience testing
  - Platform-specific metric generation (NVIDIA, AMD, Apple Silicon, Jetson, Intel client GPU, Intel Gaudi, Google TPU, Tenstorrent, Rebellions, Furiosa)
  - Background metric updates with realistic variations
  - Set `ALL_SMI_MOCK_VGPU=1` to simulate NVIDIA vGPU SR-IOV data without real vGPU hardware
  - Set `ALL_SMI_MOCK_MIG=1` to simulate NVIDIA MIG (Multi-Instance GPU) data without MIG hardware
  - Set `ALL_SMI_MOCK_HARDWARE_DETAILS=1` to include extended NVIDIA hardware detail metrics (NUMA node ID, GSP firmware mode/version, NvLink remote endpoint types, GPM SM occupancy/memory bandwidth utilization, thermal thresholds, P-state); omitted by default to simulate older drivers that do not expose these APIs
- **Performance Optimized:**
  - Template-based response generation
  - Efficient memory management
  - Minimal CPU overhead

### API Mode (Prometheus Metrics)

Expose hardware metrics in Prometheus format for integration with monitoring systems:

```bash
# Start API server on TCP port
all-smi api --port 9090

# Custom update interval (default: 3 seconds)
all-smi api --port 9090 --interval 5

# Include process information
all-smi api --port 9090 --processes

# Unix Domain Socket support (Unix only)
all-smi api --socket                              # Default path
all-smi api --socket /custom/path.sock            # Custom path
all-smi api --port 9090 --socket                  # TCP + UDS simultaneously
all-smi api --port 0 --socket                     # UDS only (disable TCP)

# Access via Unix socket
curl --unix-socket /tmp/all-smi.sock http://localhost/metrics
```

**Unix Domain Socket Details:**
- Default paths: `/tmp/all-smi.sock` (macOS), `/var/run/all-smi.sock` or `/tmp/all-smi.sock` (Linux)
- Socket permissions are set to `0600` for security (owner-only access)
- Socket file is automatically cleaned up on shutdown
- Currently Unix-only (Linux, macOS); Windows support pending Rust ecosystem maturity

Metrics are available at `http://localhost:9090/metrics` (TCP) or via Unix socket and include comprehensive hardware monitoring for:
- **GPUs:** Utilization, memory, temperature, power, frequency (NVIDIA, AMD, Apple Silicon, Intel Arc/Iris Xe client GPU, Intel Gaudi, Google TPU, Tenstorrent)
- **NVIDIA hardware details:** NUMA node ID, GSP firmware mode and version, NvLink remote endpoint type per active link, GPM SM occupancy and memory bandwidth utilization (all omitted when the driver does not support the underlying API)
- **NVIDIA vGPUs:** Per-vGPU utilization, framebuffer memory, scheduler state, and SR-IOV host mode (emitted only on vGPU-enabled hosts)
- **NVIDIA MIG:** Per-GPU MIG mode status and per-MIG-instance utilization, framebuffer memory used/total (emitted only on MIG-enabled hosts)
- **CPUs:** Utilization, frequency, temperature, power (with P/E core metrics for Apple Silicon)
- **Memory:** System and swap memory statistics
- **Storage:** Disk usage information
- **Chassis:** Node-level power consumption, thermal pressure, inlet/outlet temperatures, fan speeds
- **Processes:** GPU process metrics including AMD and Intel Arc/Xe fdinfo-based tracking (with --processes flag)

For a complete list of all available metrics, see [API.md](API.md).

#### Streaming (SSE)

Alongside `/metrics`, API mode exposes two JSON endpoints that share the
same schema as the `snapshot` subcommand (`schema: 1`):

| Endpoint | Content-Type | Purpose |
|----------|--------------|---------|
| `GET /snapshot` | `application/json` | One-shot JSON dump of the latest collection cycle. |
| `GET /events` | `text/event-stream` | Server-Sent Events: one JSON frame per collection cycle. |

```bash
# One-shot JSON — same schema as `all-smi snapshot --format json`.
curl http://localhost:9090/snapshot

# Subset by section (same grammar as the snapshot CLI):
curl 'http://localhost:9090/snapshot?include=gpu,cpu'

# Pretty-print for human inspection:
curl 'http://localhost:9090/snapshot?pretty=1'

# Live stream, one event per collection cycle. Use `-N` to disable
# curl's output buffering.
curl -N http://localhost:9090/events

# Throttle to one event per 5 s, ask for only GPUs.
curl -N 'http://localhost:9090/events?include=gpu&throttle=5'
```

Both endpoints respect the existing Unix Domain Socket transport:

```bash
curl --unix-socket /tmp/all-smi.sock http://localhost/snapshot
curl -N --unix-socket /tmp/all-smi.sock http://localhost/events
```

Query parameters for `/events`:

| Parameter | Default | Meaning |
|-----------|---------|---------|
| `include` | `gpu,cpu,memory,chassis` | Comma-separated sections to emit. |
| `throttle` | = collection interval | Minimum seconds between emitted events (clamped to ≥ interval). |
| `heartbeat` | `30` | Keep-alive `: keep-alive` interval in seconds. |

Each SSE frame is emitted as:

```
event: snapshot
id: 2026-04-20T12:34:56Z
data: {"schema":1,"timestamp":"2026-04-20T12:34:56Z","hostname":"…","gpus":[…],…}

```

When a client falls behind the server's broadcast buffer, the stream
inserts a synthetic `event: lag\ndata: {"dropped": N}\n\n` frame and
automatically resumes with the freshest live frame. `Last-Event-ID` is
accepted for reconnect but never replays history — clients resume with
the next live frame. Reverse proxies should disable buffering (nginx:
`proxy_buffering off;`); the response already advertises
`X-Accel-Buffering: no` to cover the common case.

An HTML+JS demo client is available at
[`examples/sse_client.html`](examples/sse_client.html) — open it in a
browser with `all-smi api` running on `localhost:9090`.

#### Security notes for SSE/snapshot endpoints

| Env var | Default | Effect |
|---------|---------|--------|
| `ALL_SMI_API_CORS_ALLOWED_ORIGINS` | (empty — no CORS) | Comma-separated origins permitted to read `/metrics`, `/snapshot`, and `/events` cross-origin. Set to `*` to allow all origins (logs a warning); omit for same-origin-only access. |
| `ALL_SMI_API_MAX_SSE_SUBSCRIBERS` | `256` | Cap on concurrent `/events` subscribers. Clients beyond the cap receive `503 Service Unavailable` with `Retry-After: 5`. Set to `0` to disable the cap. |

**Process label caps.** `command`, `process_name`, and `user` fields in
`/snapshot` and SSE `/events` are truncated at 256 / 128 / 128 bytes
respectively on all output surfaces (matching the Prometheus `/metrics`
exporter). Longer strings appear with a trailing `...(N bytes truncated)`
marker. This bounds response-size amplification and limits the blast radius
of secrets that may appear in argv.

**Single-flight stale fallback.** When the cached frame in `/snapshot` is
older than `2 × collection_interval`, the handler serialises a fresh
hardware collection behind a mutex (single-flight pattern). Concurrent
requests against a freshly-started or stalled server therefore share one
blocking collect rather than each spawning their own reader set.

### Scripting / CI (Snapshot Mode)

The `snapshot` subcommand emits a single, one-shot machine-readable dump of
the current hardware state to stdout (or a file) and exits. It is designed
for shell piping, CI probes, Slurm prolog/epilog hooks, and any tool that
wants `nvidia-smi --query-gpu=... --format=csv` ergonomics without starting
a long-running HTTP server.

```bash
# Default: pretty-printed JSON when stdout is a TTY, compact when piped.
all-smi snapshot

# JSON piped to jq — pretty-print auto-off avoids shell pipeline churn.
all-smi snapshot --format json | jq '.gpus[] | {name, utilization, temperature}'

# CSV with nvidia-smi-style columns. Scope to GPUs with --include gpu to
# keep the rows focused; the default CSV includes one row per CPU/memory
# /chassis device too, which is usually not what you want for GPU tooling.
all-smi snapshot --format csv --include gpu \
  --query index,name,utilization,used_memory,total_memory,temperature,power_consumption

# Prometheus exposition matching a single /metrics scrape.
all-smi snapshot --format prometheus > /tmp/snapshot.prom

# Take three samples one second apart, write a JSON array to a file.
all-smi snapshot --samples 3 --interval 1 --output /tmp/snapshot-series.json

# Include opt-in expensive sections (processes, storage).
all-smi snapshot --include gpu,cpu,memory,chassis,process,storage

# Exit non-zero when a reader times out hard enough to collect nothing.
if ! all-smi snapshot --timeout-ms 2000 --format json >/dev/null; then
    echo "no devices could be read"
    exit 1
fi
```

**Exit codes**

| Code | Meaning                                                                   |
|------|---------------------------------------------------------------------------|
| `0`  | Success. Output was written; the `errors` array may contain partial failures. |
| `1`  | Hard failure. No devices were collected from any reader.                  |
| `2`  | Flag parse error (invalid `--include`, unknown format, etc.).             |

**Slurm prolog example** — fail the prolog when a GPU is too hot to accept a
job, using `jq` to enforce a temperature cap:

```bash
#!/usr/bin/env bash
set -euo pipefail

TEMP_MAX=85
JSON="$(all-smi snapshot --format json --include gpu)"
HOT_COUNT="$(echo "$JSON" | jq "[.gpus[] | select(.temperature >= $TEMP_MAX)] | length")"
if [[ "$HOT_COUNT" -gt 0 ]]; then
    echo "Refusing to start: $HOT_COUNT GPU(s) at or above ${TEMP_MAX}C" >&2
    echo "$JSON" | jq '.gpus[] | {name, temperature}' >&2
    exit 1
fi
```

**CSV → awk pipeline** — compute average utilization across all GPUs:

```bash
all-smi snapshot --format csv --include gpu --query utilization \
  | awk -F, 'NR > 1 { sum += $1; n++ } END { if (n) printf "avg=%.1f%%\n", sum/n }'
```

**Options summary**

| Flag              | Default                           | Description                                      |
|-------------------|-----------------------------------|--------------------------------------------------|
| `--format`        | `json`                            | `json` / `csv` / `prometheus`.                   |
| `--pretty`        | auto (on when stdout is a TTY)    | Force pretty-print on or off for JSON.           |
| `--include`       | `gpu,cpu,memory,chassis`          | Comma-separated sections to collect.             |
| `--query`         | section-specific defaults         | CSV columns as comma-separated dot paths.        |
| `--samples`       | `1`                               | Number of samples to collect.                    |
| `--interval`      | `0`                               | Seconds between samples (only if `--samples > 1`). |
| `--timeout-ms`    | `5000`                            | Per-reader timeout in milliseconds.              |
| `--output` / `-o` | stdout                            | Write to this path (`-` also means stdout). On Unix: created with mode `0600` (owner-only), symlinks refused, atomic write via sibling `.tmp` + rename. |

### Recording & Replay

The `record` subcommand captures a live metric stream to disk as NDJSON, and
`view --replay <file>` plays it back through the same TUI the operator would
have seen live. Intended for post-hoc incident investigation without a
Prometheus retention store — operators can rewind to the moment throughput
cratered and see the exact GPU/CPU/memory/chassis state at that tick.

Each captured frame uses the same JSON shape as `snapshot --format json`
(same serializer), plus an optional header frame and sparse index frames
every 1000 data frames to enable fast seeking.

```bash
# Capture 30 seconds of local hardware state to a zstd-compressed file.
all-smi record --output incident.ndjson.zst --duration 30s --interval 1

# Record until SIGTERM; rotate at 100MB per segment, keep last 10 files.
all-smi record --output trace.ndjson.zst --max-size 100M --max-files 10

# Record remote cluster scrapes (same HTTP path as `view`).
all-smi record --source remote \
  --hosts http://gpu-node1:9090 http://gpu-node2:9090 \
  --output cluster.ndjson.gz --compress gzip --duration 1h

# Replay a captured file — identical TUI to the live view.
all-smi view --replay incident.ndjson.zst

# Start playback at 14:32 (HH:MM:SS) and loop.
all-smi view --replay incident.ndjson.zst --start 00:14:32 --loop

# Replay at 4x speed.
all-smi view --replay incident.ndjson.zst --speed 4.0
```

**Replay-mode keybindings** (active only with `--replay`):

| Key          | Action                                               |
|--------------|------------------------------------------------------|
| `SPACE`      | Play / pause                                         |
| `]` / `[`    | Step one frame forward / back (auto-pauses)          |
| `+` / `-`    | Cycle speed through `0.25, 0.5, 1.0, 2.0, 4.0, 8.0`  |
| `j` / `k`    | Seek -10s / +10s                                     |
| `g`          | Open timecode editor (`HH:MM:SS` then Enter)         |
| `L`          | Toggle loop playback                                 |
| `q` / `Esc`  | Exit replay                                          |

The status bar shows `REPLAY | HH:MM:SS | frame N / M | Xx | playing/paused`.
Filter-edit mode (`/`) still takes precedence over replay keys, so the
operator can filter the visible GPUs mid-playback.

**`record` options summary**

| Flag              | Default                           | Description                                      |
|-------------------|-----------------------------------|--------------------------------------------------|
| `--output` / `-o` | platform cache dir + `all-smi/records/all-smi-record.ndjson.zst` [^cache] (override via `record.output_dir` config, or `-o` flag) | Output path. Extension picks the codec. |
| `--interval` / `-i` | `3`                             | Seconds between frames.                          |
| `--duration`      | `0` (= record until SIGTERM)      | Accepts `30s`, `5m`, `1h`, `1d`, or bare seconds. |
| `--source`        | `local`                           | `local` (hardware readers) or `remote` (HTTP scrape). |
| `--hosts` / `--hostfile` | (none)                     | Required when `--source=remote`.                 |
| `--include`       | `gpu,cpu,memory,chassis`          | Comma-separated sections (plus `process`).       |
| `--max-size`      | `100M`                            | Rotation threshold per segment (`1K`, `10M`, `2G`). `0` disables rotation. |
| `--max-files`     | `10`                              | Max segments on disk (active + rotated).         |
| `--compress`      | auto (from extension)             | `zstd` / `gzip` / `none`.                        |

**Security invariants**

`record` and `view --replay` apply the following hardening that cannot be
disabled:

- **Writer — symlink refusal.** On Unix the output file is opened with
  `O_NOFOLLOW` and mode `0600`. A pre-existing symlink at the target path
  causes `record` to exit immediately rather than follow the link. The same
  check applies when rolling over to a numbered segment: if a symlink is found
  at the rollover path, the rotation is aborted.
- **Replay — per-line size cap.** Each decompressed NDJSON line is limited to
  16 MiB. A `.zst` or `.gz` file that expands a single line beyond this limit
  (decompression-bomb attack) is treated as a corrupted line: it is skipped
  with a warning and reading continues from the next line.
- **Replay — zstd window ceiling.** The zstd decoder is configured with a
  128 MiB window-log ceiling. A stream declaring a larger window is rejected
  before decompression begins.
- **Replay — hosts list cap.** A replay header advertising more than 1024 host
  entries is truncated to 1024 at parse time so the TUI tab row cannot be
  overwhelmed by a hostile recording.

### Quick Start with Make Commands

For development and testing, you can use the provided Makefile:

```bash
# Run local monitoring
make local

# Run remote view mode with hosts file
make remote

# Start mock server for testing
make mock

# Build release version
make release

# Run tests
make test
```

## Library API

`all-smi` can also be used as a Rust library for building custom monitoring tools or integrating hardware metrics into your applications.

### Add Dependency

Add to your `Cargo.toml`:

```toml
[dependencies]
all-smi = "0.15"
```

### Basic Usage

```rust
use all_smi::{AllSmi, Result};

fn main() -> Result<()> {
    // Initialize with auto-detection
    let smi = AllSmi::new()?;

    // Get all GPU/NPU information
    for gpu in smi.get_gpu_info() {
        println!("{}: {}% utilization, {:.1}W",
            gpu.name, gpu.utilization, gpu.power_consumption);
    }

    // Get CPU information
    for cpu in smi.get_cpu_info() {
        println!("{}: {:.1}% utilization", cpu.cpu_model, cpu.utilization);
    }

    // Get memory information
    for mem in smi.get_memory_info() {
        println!("Memory: {:.1}% used", mem.utilization);
    }

    Ok(())
}
```

### Using the Prelude

For convenience, import all common types:

```rust
use all_smi::prelude::*;

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    // Types like GpuInfo, CpuInfo, MemoryInfo are available
    let gpus: Vec<GpuInfo> = smi.get_gpu_info();
    println!("Found {} GPU(s)", gpus.len());

    Ok(())
}
```

### Available Methods

| Method | Returns | Description |
|--------|---------|-------------|
| `get_gpu_info()` | `Vec<GpuInfo>` | GPU/NPU metrics (utilization, memory, temp, power) |
| `get_cpu_info()` | `Vec<CpuInfo>` | CPU metrics (utilization, frequency, temp) |
| `get_memory_info()` | `Vec<MemoryInfo>` | System memory metrics |
| `get_process_info()` | `Vec<ProcessInfo>` | GPU process information |
| `get_chassis_info()` | `Option<ChassisInfo>` | Node-level power and thermal info |
| `get_vgpu_info()` | `Vec<VgpuHostInfo>` | NVIDIA vGPU host and per-instance metrics (empty on non-vGPU hosts) |
| `get_mig_info()` | `Vec<MigGpuInfo>` | NVIDIA MIG per-GPU mode status and per-instance metrics (empty on non-MIG hosts) |

### Configuration

```rust
use all_smi::{AllSmi, AllSmiConfig};

let config = AllSmiConfig::new()
    .sample_interval(500)  // 500ms sample interval
    .verbose(true);        // Enable verbose warnings

let smi = AllSmi::with_config(config)?;
```

### Thread Safety

`AllSmi` is `Send + Sync` and can be safely shared across threads:

```rust
use std::sync::Arc;
use std::thread;

let smi = Arc::new(AllSmi::new()?);

let smi_clone = smi.clone();
thread::spawn(move || {
    let gpus = smi_clone.get_gpu_info();
    // ...
});
```

For more examples, see `examples/library_usage.rs` in the repository.

## Development

For development documentation including building from source, testing with mock servers, architecture details, and technology stack information, see [DEVELOPERS.md](DEVELOPERS.md).

## Testing

For comprehensive testing documentation including unit tests, integration tests, and shell script tests, see [TESTING.md](TESTING.md).

### Quick Test Commands
```bash
# Run all unit tests (no sudo required)
cargo test

# Run tests including those requiring sudo (macOS only)
sudo cargo test -- --include-ignored

# Run shell script tests for containers and real-world scenarios
cd tests && make all
```

## Contributing

Contributions are welcome! Areas for contribution include:

- **Platform Support:** Additional GPU vendors or operating systems
- **Features:** New metrics, visualization improvements, or monitoring capabilities
- **Performance:** Optimization for larger clusters or resource usage
- **Documentation:** Examples, tutorials, or API documentation

Please submit pull requests or open issues for bugs, feature requests, or questions.

## Acknowledgments

This project is being developed with tremendous help from [Claude Code](https://claude.ai/code) and [Gemini CLI](https://github.com/google-gemini/gemini-cli). These AI-powered development tools have been instrumental in accelerating the development process, improving code quality, and implementing complex features across multiple hardware platforms.

The journey of building all-smi with AI assistance has been a fascinating exploration of how domain expertise guides AI capabilities. From the initial three-day Rust learning sprint with Google AI Studio and ChatGPT to the recent development with Gemini CLI and Claude Code, this project demonstrates that the boundary of AI coding capability is tightly bound by the expertise of the person guiding it. [Read the full development story here](docs/AI_DEVELOPMENT_STORY.md).

## License

This project is licensed under the Apache License 2.0.  
See the [LICENSE](./LICENSE) file for details.

## Changelog

### Recent Updates
- **v0.25.0 (2026/07/31):** Cut local-mode collection cost with continuous IOReport sampling and a parallelized collection pipeline, give local mode its own polling cadence instead of the remote one, and make Activity history graphs scroll in time instead of shrinking as history accumulates
- **v0.24.2 (2026/07/24):** Report VRAM total, VRAM used, and power draw for Intel Battlemage GPUs on the mainline `xe` driver, which does not expose the `tile0/vram0` sysfs counters the `i915` path relies on
- **v0.24.1 (2026/07/20):** Replace the yanked `aes` and `crypto-bigint` crates (pulled in transitively through `russh`) with their latest non-yanked releases, and bump `russh` to 0.62 and `tower-http` to 0.7, moving several transitive crypto crates from prerelease to stable
- **v0.24.0 (2026/07/20):** Redesign the local Activity panel history graphs for readability: btop-style multi-row braille graphs with bottom-to-top height-gradient colors on tall terminals, soft auto-ranging and trend glyphs (up/down arrows) for single-row sparklines, bucket max-pooling resampling that preserves peaks, and width-safe panel rendering
- **v0.23.1 (2026/07/20):** Fix Prometheus scrape failures from unsanitized dynamic label names, add Intel Xe GPU utilization computed from the Xe kernel driver's gtidle idle-residency, support Xe hwmon temperature sensor numbering (temp2-based), and raise the minimum Rust version to 1.96 (MSRV)
- **v0.23.0 (2026/06/27):** Migrate the Tenstorrent reader to the upstream-published luwen 0.8.5 crates (supported architectures now Wormhole and Blackhole), suppress console windows on Windows subprocess spawns, and bump sysinfo to 0.39. **BREAKING**: Tenstorrent Grayskull is no longer supported (detected and skipped instead of aborting); building from source now requires Rust 1.95 (MSRV)
- **v0.22.0 (2026/05/27):** Harden the release workflow with a corrected macOS notarization key decode, self-healing rebuild and re-notarization of past tags, and a per-OS build target selector; document Intel client GPU support (Arc/Iris Xe) across the README, `--help` output, and developer docs
- **v0.21.1 (2026/05/27):** Add Intel client GPU monitoring (Arc/Iris Xe) on Windows and Linux with an opt-in Level Zero backend, fdinfo-based per-process memory, and engine-busy utilization; add notarized macOS and code-signed Windows release binaries
- **v0.21.0 (2026/05/26):** Major release adding `snapshot`/`record`/`view --replay`/`config`/`doctor` subcommands, cluster-wide Users (`V`) and Topology (`T`) tabs, agentless SSH transport, TOML config support, energy/cost accounting, filter query (`/`) and threshold alerts (`A`), and NVIDIA vGPU/MIG/extended thermal monitoring. **BREAKING**: rename Prometheus labels `index`/`uuid` to `gpu_index`/`gpu_uuid` (NVIDIA) and `npu_index`/`npu_uuid` (other NPUs); old names still accepted.
- **v0.20.1 (2026/04/10):** Fix local header metric row jitter by using fixed-width formatted fields; auto-promote pre-release to release in CI
- **v0.20.0 (2026/04/10):** Redesign local-mode TUI with Activity panel featuring braille sparklines, CPU per-core view, host summary bar, and per-node LED grid; add Apple M5 Pro/Max Super core (S-CPU) support
- **v0.19.0 (2026/04/08):** Fix Apple Silicon SMC float decoding to restore real CPU/GPU die temperatures, cache platform detection to avoid per-frame system_profiler on macOS, and fix TIME+/Command column alignment in process list
- **v0.18.1 (2026/04/08):** Fix TUI responsiveness over SSH with non-blocking flush, eliminate 1-second per-frame stall from RuntimeEnvironment::detect(), drain key events after render, and cache per-frame filesystem reads
- **v0.18.0 (2026/04/07):** Reduce TUI idle CPU with event-driven wakeups, snapshot-based rendering, cached view data, and trimmed hot-path overhead; fix scroll calculation and render throttle for cursor/scroll input
- **v0.17.6 (2026/04/06):** Bump hyper 1.9, nvml-wrapper 0.12.1, libamdgpu_top 0.11.3 and update GitHub Actions to Node.js 24
- **v0.17.5 (2026/03/29):** Bump dependencies including nvml-wrapper 0.12 and fix yanked uds_windows
- **v0.17.4 (2026/03/29):** Feature-gate CLI/TUI deps behind `cli` feature for lighter library builds, fix Furiosa RNGD support for latest SDK & driver APIs
- **v0.17.3 (2026/03/04):** Fix multi-GPU process duplication, upgrade breaking dependencies (rand, reqwest, sysinfo, whoami)
- **v0.17.2 (2026/02/08):** Fix file descriptor leaks in Jetson, Tenstorrent, and NVIDIA readers by using global system instance
- **v0.17.1 (2026/02/08):** Fix file descriptor leak in API mode by reusing resource handles
- **v0.17.0 (2026/01/13):** Add GPU process filter toggle ('f' key) and improve process list sort stability
- **v0.16.0 (2026/01/04):** Add proper library API for external Rust projects with high-level AllSmi client, unified error handling, and comprehensive documentation
- **v0.15.2 (2026/01/02):** Fix Rebellions NPU detection compatibility with rbln SDK 2.0.x
- **v0.15.1 (2025/12/31):** Fix memory leak in IOReportIterator on Apple Silicon by properly releasing CFDictionaryRef
- **v0.15.0 (2025/12/31):** Add Unix Domain Socket support for API mode, Windows CPU temperature fallback chain, binary size optimization, and repository organization change
- **v0.14.0 (2025/12/25):** Add Windows x64 build target, native macOS APIs for no-sudo monitoring, chassis/node-level power monitoring, and remove legacy powermetrics
- **v0.13.1 (2025/12/23):** Upgrade tonic/prost to 0.14, wmi to 0.18, libloading to 0.9, and optimize build dependencies
- **v0.13.0 (2025/12/23):** Add Google Cloud TPU monitoring support (v2-v7/Ironwood), optimize CPU utilization with improved polling and rendering
- **v0.12.0 (2025/12/07):** Add Windows build support, fix AMD GPU dependencies in Dockerfile builder stage
- **v0.11.0 (2025/11/25):** Add Intel Gaudi 3 AI accelerator support, unified AI acceleration library naming for cross-platform consistency, GPU/NPU reader caching optimization for performance, and AMD GPU driver version extraction
- **v0.10.0 (2025/11/21):** Add AMD GPU support with ROCm/libamdgpu_top integration, comprehensive security and performance review with critical fixes, refactor data collection with Strategy pattern, enhanced parsing macros, and Linux-only NPU support
- **v0.9.0 (2025/08/29):** Separate local/remote monitoring commands, Backend.AI cluster auto-discovery, modular refactoring for better maintainability, and Prometheus metric fixes
- **v0.8.0 (2025/08/08):** Container-aware resource monitoring, enhanced ARM CPU frequency detection, UI improvements for process list, license change to Apache 2.0, and PPA build enhancements
- **v0.7.2 (2025/08/06):** Reorganize man page location in release archives, add GPU core count for Apple Silicon, animated loading progress bar, and fix display issues
- **v0.7.1 (2025/08/03):** Add manpage for Debian/Ubuntu package, updated installation guide with PPA support, and fixed debian_build workflow
- **v0.7.0 (2025/08/02):** Add Furiosa RNGD NPU support, Debian/Ubuntu PPA packaging, scrolling device names, and improved CI/CD workflows
- **v0.6.3 (2025/07/28):** Add Rebellions ATOM NPU support with secure container monitoring
- **v0.6.2 (2025/07/25):** Added multi-segment bar visualization with stacked memory display, CPU temperature for Linux, CPU cache detection, per-core CPU metrics, and fixed-width CPU display formatting
- **v0.6.1 (2025/07/19):** Fixed multi-node view hanging, improved hostname handling, optimized network fetch, and updated Ubuntu release workflows
- **v0.6.0 (2025/07/18):** Added Tenstorrent NPU support, improved UI alignment and terminal resize handling, modularized API metrics, and enhanced disk filtering
- **v0.5.0 (2025/07/12):** Enhanced Apple Silicon support with ANE power in watts, P+E frequency display, thermal pressure text, interactive process sorting, and configurable PowerMetrics intervals
- **v0.4.3 (2025/07/11):** Fix P-CPU/E-CPU gauges for all Apple Silicon variants (M1/M2/M3/M4) including M1 Pro hybrid format
- **v0.4.2 (2025/07/10):** Eliminate PowerMetrics temp file growth with in-memory buffer, Homebrew installation support
- **v0.4.1 (2025/07/10):** Mock server improvements, efficient Apple Silicon and NVidia GPU support
- **v0.4.0 (2025/07/08):** Architectural refactoring, Smart sudo detection and comprehensive unit testing
- **v0.3.3 (2025/07/07):** CPU, Memory, and ANE support, and UI fixes
- **v0.3.2 (2025/07/06):** Cargo.toml for publishing and release process
- **v0.3.1 (2025/07/06):** GitHub actions and Dockerfile, and UI fixes
- **v0.3.0 (2025/07/06):** Multi-architecture support, optimized space allocation, enhanced UI
- **v0.2.2 (2025/07/06):** GPU sorting functionality with hotkeys
- **v0.2.1 (2025/07/05):** Help system improvements and code refactoring
- **v0.2.0 (2025/07/05):** Remote monitoring and cluster management features
- **v0.1.1 (2025/07/04):** ANE (Apple Neural Engine) support, page navigation keys, and scrolling fixes
- **v0.1.0 (2024/08/11):** Initial release with local GPU monitoring
