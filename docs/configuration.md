# Configuration reference

[Home](../README.md) · [Documentation](../README.md#documentation)

`all-smi` reads optional settings from a TOML config file. Every field has a compiled default, so a fresh install requires no file; operators only create one when they want persistent overrides (hostfile path, update interval, alert thresholds, `$/kWh`, etc.).

### File locations

| Platform | Canonical path | Also searched |
|----------|----------------|---------------|
| Linux    | `$XDG_CONFIG_HOME/all-smi/config.toml` (fallback `~/.config/all-smi/config.toml`) | `/etc/all-smi/config.toml` |
| macOS    | `~/Library/Application Support/all-smi/config.toml` | `~/.config/all-smi/config.toml`, then `/Library/Application Support/all-smi/config.toml` |
| Windows  | `%APPDATA%\all-smi\config.toml` | `%PROGRAMDATA%\all-smi\config.toml` |

Candidates are probed in order and the first existing file wins, so a per-user file always beats the machine-wide one. The machine-wide paths exist for daemons. On Linux a service running as a dedicated account has no home directory, so no per-user candidate ever resolves for it; on macOS a launchd LaunchDaemon runs outside any login session, so `~/Library/Application Support` resolves to root's home rather than the administrator's, and launchd has no environment-file mechanism to work around it; on Windows a service running as LocalSystem resolves `%APPDATA%` into `C:\Windows\System32\config\systemprofile\AppData\Roaming`, which no operator will ever edit (see [Running as a service](services.md)). `all-smi config init` only ever writes the per-user path; creating `/etc/all-smi/config.toml`, `/Library/Application Support/all-smi/config.toml`, or `%PROGRAMDATA%\all-smi\config.toml` is an administrator's decision.

Pass `--config <PATH>` to any subcommand to override the discovery and force a specific file. A missing or malformed `--config` target is a hard error (exit 2); implicit discovery silently falls back to defaults when no candidate file exists. To print the active path for the current user without writing any file, run `all-smi config path` (also surfaced in `all-smi --help` under the "Configuration file" section).

### Precedence

Highest to lowest: **CLI flag > environment variable > config file > compiled default.** For example, `--port 9091` beats `ALL_SMI_API_PORT=9200` beats `[api] port = 9300` in `config.toml` beats the compiled default of `9090`. Env-var names follow the canonical pattern `ALL_SMI_<SECTION>_<KEY>` in upper-snake; legacy aliases from earlier releases (`ALL_SMI_ALERT_TEMP`, `ALL_SMI_ENERGY_PRICE`, etc.) keep working.

The snapshot format and some SSH flags have current precedence exceptions; see [Limitations](#limitations).

### Helpers

- `all-smi config init [--force]` writes a commented example config to the platform-canonical path. Refuses to overwrite without `--force`. The file is created with `O_NOFOLLOW` and mode `0o600` on Unix.
- `all-smi config print [--format toml|json] [--show-secrets]` prints the fully merged effective configuration. `webhook_url` is redacted unless `--show-secrets` is passed.
- `all-smi config validate [<path>] [--strict]` parses a config file and reports any errors (with line/column on parse failures). Exit 0 valid, 2 invalid. `--strict` rejects unknown keys.
- `all-smi config path [--json]` prints the active config-file path with an `(active)` / `(not found)` marker, plus the candidate search order. Read-only — no file is created. The same active path is shown in `all-smi --help` under the "Configuration file" block.

### Reload

Config reload is not supported — restart the process to pick up changes. This keeps the Prometheus counter and WAL state semantics simple.

### Schema

The canonical schema carries `schema_version = 1` at the top level and nine sections. Unknown keys are tolerated by default (forward compat) and reported by `config print`; `config validate --strict` rejects them. Future schema versions produce a hard error instead of silently loading.

**`[general]`** — cross-mode defaults

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `default_mode` | string | `"local"` | `ALL_SMI_GENERAL_DEFAULT_MODE` | Which subcommand runs when none is specified: `"local"`, `"view"`, or `"api"`. |
| `theme` | string | `"auto"` | `ALL_SMI_GENERAL_THEME` | Reserved setting (currently not applied to the TUI): `"auto"`, `"light"`, `"dark"`, `"high-contrast"`, `"mono"`. |
| `locale` | string | `"en"` | `ALL_SMI_GENERAL_LOCALE` | Display locale setting. It is accepted but currently has no effect; output remains in English. |

**`[local]`** — options for `all-smi local`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `interval_secs` | integer | adaptive | `ALL_SMI_LOCAL_INTERVAL_SECS` | Collection interval in seconds. Omit for adaptive pacing; `0` removes the collection-loop sleep. |

**`[view]`** — options for `all-smi view`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `hostfile` | string | — | `ALL_SMI_VIEW_HOSTFILE` | Path to a CSV/newline file listing remote `http://host:port` endpoints. |
| `hosts` | array of strings | `[]` | `ALL_SMI_VIEW_HOSTS` (comma-separated) | Inline list of remote endpoints (alternative to `hostfile`). |
| `interval_secs` | integer | adaptive | `ALL_SMI_VIEW_INTERVAL_SECS` | Scrape interval in seconds. Omit for adaptive (based on host count); `0` removes the collection-loop sleep. |

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
| `enabled` | bool | — | — | Accepted but ignored; currently does not disable alerts. |
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
| `wal_path` | string | platform cache dir + `all-smi/energy-wal.bin` ([cache paths](#cache-paths)) | `ALL_SMI_ENERGY_WAL_PATH` | Path to the energy write-ahead log for persistent kWh accumulation. Operator overrides support `~` expansion; when unset, the platform cache directory is used. |
| `gap_interpolate_seconds` | integer | `10` | `ALL_SMI_ENERGY_GAP_INTERPOLATE_SECONDS` | Max gap (1–3600 s) to interpolate across when the WAL has a hole. |
| `wal_enabled` | bool | `true` | `ALL_SMI_ENERGY_WAL_ENABLED` | Enable the WAL. Disable in read-only container environments. |

**`[display]`** — reserved TUI settings

These settings are loaded and stored but the current renderers do not read them; changing them does not alter the display.

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `color_scheme` | string | `"default"` | `ALL_SMI_DISPLAY_COLOR_SCHEME` | Colour palette: `"default"`, `"colorblind"`, `"mono"`. |
| `gauge_style` | string | `"blocks"` | `ALL_SMI_DISPLAY_GAUGE_STYLE` | Bar-chart fill style: `"blocks"` or `"braille"`. |
| `show_led_grid` | bool | `true` | `ALL_SMI_DISPLAY_SHOW_LED_GRID` | Reserved LED-grid toggle. |

**`[record]`** — defaults for `all-smi record`

| Key | Type | Default | Env var | Description |
|-----|------|---------|---------|-------------|
| `output_dir` | string | platform cache dir + `all-smi/records` ([cache paths](#cache-paths)) | `ALL_SMI_RECORD_OUTPUT_DIR` | Directory where recording segments are written. Operator overrides support `~` expansion; when unset, the platform cache directory is used. |
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

### Cache paths

The platform cache directory is `$XDG_CACHE_HOME` (or `~/.cache`) on Linux, `~/Library/Caches` on macOS, and `%LOCALAPPDATA%` on Windows. Record output, the energy WAL, and users-CSV exports use this location consistently when no explicit path is configured.

### SSH settings

The `[view]` section also accepts `ssh` (array of targets), `ssh_hostfile`, `ssh_key`, `ssh_config`, `ssh_strict_host_key`, `ssh_timeout_secs`, `ssh_fallback`, `ssh_known_hosts`, and `ssh_concurrency`. These keys do not have environment-variable overlays. See [SSH usage](usage.md#agentless-ssh-mode) for their meanings. `ssh_config` is accepted but not applied.

### Limitations

- `alerts.enabled = false` does not disable alerts; this key is currently a no-op.

- `general.theme`, `general.locale`, and the `[display]` settings are accepted and printed but are not currently applied by the TUI renderers.
- An explicit `snapshot --format json` does not override a non-JSON `snapshot.default_format`; the current CLI cannot distinguish that flag from its compiled default. Set `snapshot.default_format = "json"` in the selected config when a script requires JSON.
- Explicit SSH values equal to the CLI defaults (`--ssh-strict-host-key yes`, `--ssh-timeout-secs 10`, `--ssh-concurrency 32`) can be replaced by the corresponding config values. Check your selected config, especially the host-key policy.
- SSH keys are accepted by the loader but currently reported as unknown by `config validate --strict`; `config print` does not include them.
