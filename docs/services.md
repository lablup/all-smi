# Running as a service

[Home](../README.md) · [Documentation](../README.md#documentation)

`all-smi api` is the data source behind `all-smi view --hosts/--hostfile`, so on a cluster it wants to start at boot, restart on failure, and log to the platform's journal. The `all-smi service` subcommand installs and controls it:

```bash
all-smi service install   [--user] [--service-user NAME] [--now] [--force]
all-smi service uninstall [--user] [--force]
all-smi service start     [--user]
all-smi service stop      [--user]
all-smi service restart   [--user]
all-smi service status    [--user] [--json]
```

The default scope is system-wide and requires root on Unix or Administrator rights on Windows. On Linux and macOS, `--user` installs a per-user service with no elevation. `status` exits `0` when the service is running and `3` when it is stopped or not installed, mirroring `systemctl is-active`; every other failure exits `1`.

There are deliberately no `--port` or `--interval` flags on `install`. Runtime configuration lives in the environment file and the TOML config file, so changing a setting never means regenerating and reinstalling the service definition.

## Linux (systemd)

The canonical unit is [`packaging/systemd/all-smi.service`](../packaging/systemd/all-smi.service). Both installation paths use it: the Debian package ships it verbatim, and `all-smi service install` embeds the same file and rewrites `ExecStart=` plus the account directives for the scope you asked for.

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

**Unix socket.** `PrivateTmp=true` namespaces `/tmp`, so the default `/tmp/all-smi.sock` fallback would not be reachable from outside the service. The unit provides `/run/all-smi` through `RuntimeDirectory=`; set `ALL_SMI_API_SOCKET=/run/all-smi/all-smi.sock` in the environment file to expose the socket there.

**Other init systems.** OpenRC, runit, and sysvinit are not supported by the subcommand; it detects the absence of systemd and points at the canonical unit for manual adaptation.

## macOS (launchd)

The canonical job definition is [`packaging/launchd/com.lablup.all-smi.plist`](../packaging/launchd/com.lablup.all-smi.plist), a self-contained system LaunchDaemon you can also copy into `/Library/LaunchDaemons` by hand. `all-smi service install` embeds the same file and rewrites `ProgramArguments`, the log paths, and the account keys for the scope you asked for. The launchd label is `com.lablup.all-smi` in both scopes; they live in different domains, so the name never collides.

**From Homebrew.** The formula ships a service block, so `brew services` is the supported path and the subcommand refuses to install alongside it. `brew install` only writes the plist into the keg; it never calls `launchctl` for you, so the exporter is not listening until you run one of the `brew services start` commands below:

```bash
brew install all-smi

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

`--service-user` sets `UserName` in system scope and drops `GroupName` rather than mirroring the account name the way the Linux renderer does. macOS has no convention that an account owns an eponymous group, so omitting the key makes launchd use the account's primary group straight from the password database. Give such an account a writable home directory, or point `[energy] wal_path` somewhere it can write, or the energy WAL degrades to in-memory with a warning in the log.

**Apple Silicon metrics under launchd.** The native readers (IOReport, the SMC, and `NSProcessInfo.thermalState`) need no GUI session and no sudo, and a LaunchAgent exports the same metric set a foreground `all-smi api` does: GPU utilization and power, SMC GPU and CPU temperatures, ANE power, thermal pressure, and P/E cluster frequencies. The plist sets `ProcessType` to `Background` so the exporter runs at background QoS on the efficiency cores and does not compete with the workload it is watching. That costs a few seconds of extra startup, because enumerating the IOReport channel list is the expensive part of coming up; it is a one-time cost per launch.

## Windows (Service Control Manager)

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

The service is named `all-smi` and appears in `services.msc` as **all-smi GPU/NPU Metrics Exporter**. It defaults to **LocalSystem** for service-mode sensor access; interactive monitoring does not universally require this account. Start type is plain automatic rather than delayed, so metrics exist early in boot. If the process dies, the SCM restarts it after 5 seconds, up to three times before the failure counter resets a day later.

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

## Troubleshooting and readiness

A running service has bound its listener but may still be collecting its first sample. Check [`/-/ready`](../API.md#get--ready) before depending on telemetry. Use the platform log paths above and [doctor](troubleshooting.md) to diagnose missing devices. For sandbox directives and account handling, see [service design notes](ARCHITECTURE.md#service-isolation).
