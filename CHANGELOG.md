# Changelog

[Home](README.md) · [Release downloads](https://github.com/lablup/all-smi/releases)

## v0.29.1 (2026/11/24)

v0.29.0 became the latest release without its Windows ARM64 and Intel macOS archives, and its Homebrew and Debian package updates never ran because its Release run ended cancelled. This release is the first 0.29 release on Homebrew and carries every change listed under v0.29.0 below.

- Removed the Intel macOS (`x86_64-apple-darwin`) release archive. `all-smi-macos-x86_64.zip` is no longer built or published, and the Homebrew updater refuses to bump a tap formula that still has an Intel macOS stanza. Intel Macs remain supported at runtime; install with `cargo install all-smi` or build from source. v0.29.0 and earlier keep their Intel archives (#518).
- Moved the GitHub Actions in every workflow to their latest Node 24 majors: `actions/checkout` v7, `actions/cache` v6, `actions/setup-python` v7, and `softprops/action-gh-release` v3 (#516).

[Full changelog](https://github.com/lablup/all-smi/compare/v0.29.0...v0.29.1)

## v0.29.0 (2026/11/23)

- Added N-tier CPU topology on Linux and Windows ARM. Linux detects tiers once per reader from Intel hybrid PMU cpumasks, arm64 MIDR part numbers, or the firmware `cpu_capacity` table (honoring the container cpuset), Windows ARM reports P/M/E clusters, and the local TUI renders every tier (#474).
- Added the `all_smi_cpu_tier_core_count`, `all_smi_cpu_tier_utilization`, `all_smi_cpu_tier_frequency_mhz` and `all_smi_cpu_tier_l2_cache_mb` metrics and a `cpu_tier` label on `all_smi_cpu_core_utilization`. Tier frequency and cache are reported only on Apple Silicon; existing metric names and the Apple s/p/e series are unchanged (#474).
- Added the `ALL_SMI_CPU_TEMP_SENSOR` override for the Linux CPU temperature sensor (`<driver>:temp<N>`, `<driver>:<label>`, or an absolute `/sys` path); `all-smi doctor` shows which sensor was picked and whether the override took effect (#496).
- Added Linux CPU temperature selection from motherboard sensor chips by `tempN_label` (PECI, AMD TSI, `CPU Package`, `SoC Temperature` on Ampere, and others), from the kernel `temp*_type` value on unlabelled Super I/O chips, and from ARM thermal zone types (#496, #498).
- Added the `amd.gpu_metrics.revision` doctor check, which warns when a card's `gpu_metrics` revision is newer than the bundled parser reads and names the fallbacks in use (#481).
- Added `VRAM used/total` to the local TUI GPU row (#469).
- Fixed the AMD GPU process list being empty on every Linux host that had a GPU process: the plugin computed memory percent against an unrefreshed total and sent `null`, which the host rejected (#469).
- Fixed AMD utilization reading 0% on drivers that emit `gpu_metrics` newer than the parser supports, such as MI300X with v1.9; it falls back to sysfs `gpu_busy_percent` (#469).
- Fixed MI300 temperature reading 0 °C on boards without an edge sensor; it falls back to the junction sensor (#469).
- Fixed doctor reporting "ROCm not detected" on ROCm 7.x; the version now comes from `librocm-core`, then the legacy version files, then the 7.x `core-*` files (#469).
- Fixed the header showing `0°C` on hosts without a CPU temperature sensor (now `Tmp N/A`) and the GPU Metrics panel truncating power range badges wider than 5 characters (#469).
- Fixed a base M5 being reported as Super + Performance with no efficiency cores, which dropped its E-cluster frequency and L2 cache; Apple Silicon tiers now come from every perflevel name (#472).
- Fixed the Linux CPU temperature reader taking the first of four fixed sysfs paths, which could report a motherboard sensor; it selects `coretemp`, `k10temp`/`zenpower` or `cpu_thermal` by driver name and resolves the sensor once per process (#493).
- Automatic Linux CPU temperature selection no longer prefers `asus_wmi_sensors`, whose BIOS can stop or pin fans when polled often (#498).
- Fixed NVML being initialized and shut down each time an `AllSmi` was built and dropped. One NVML handle is shared per process, and a failed init backs off from 30 s to 5 min while readers use the `nvidia-smi` fallback (#490).
- Fixed the macOS GPU core estimate reading a CPU model cached only after the call; the estimate is now parsed from the chip generation and tier (#507).
- Fixed the hl-smi monitor thread leaking on shutdown; `shutdown()` now wakes and joins it (#507).
- Fixed hosts added by a hostfile reload getting no tab, `record --hostfile` scraping nothing, a `~/` hostfile path breaking reloads, and a stale "connected" status after a deadline abort in remote mode (#509).
- Fixed installing the Debian package on a host whose NVIDIA driver came from NVIDIA's CUDA repository replacing that driver: the driver moved from `Recommends` to `Suggests`. The docs warn that `--install-suggests` installs it again (#476, #480).
- Remote collection bounds each tick: 1 s connect and 3 s request timeouts inside a 4 s budget, no retry after a timeout, and per-host backoff from 2 s to 60 s shown as `backing-off`. Bodies are parsed in each host task without a regex, the hostfile is re-read only when it changes, and the line cap rose from 1024 bytes to 32 KiB (#509).
- The TUI no longer deep-clones `AppState` every frame, and local mode idles at the 1 s tick instead of a permanent 5 Hz tick when nothing animates; mouse motion no longer triggers redraws (#506).
- Windows: one WMI worker thread owns a cached connection per namespace, CPU temperature sources back off for 30 s to 300 s after 3 consecutive failures, and `Win32_VideoController` and `Win32_Processor` results are cached (#507).
- Cut per-tick Linux CPU collection work: the machine string comes from `uname(2)` once instead of spawning `uname -m` every sample, the static `/proc/cpuinfo` fields are parsed once when the reader is built, the current frequency is read from `scaling_cur_freq` (or only the `cpu MHz` lines when cpufreq is absent), and the CPU reader no longer reads `/proc/stat` itself (#504).
- Cut per-tick process collection work: GPU readers refresh only CPU, memory, user and command line instead of `ProcessRefreshKind::everything()`, priority/nice and user names are cached per process, the merge with GPU rows no longer clones every baseline row, and the AMD and Intel readers on Windows and the Intel reader on Linux return their GPU rows to the TUI without a second full process pass (#504).
- Fixed the API and library process lists from the NVIDIA, Jetson, Tenstorrent, Intel and Windows GPU readers containing one row per thread; they now list processes only (#504).
- Fixed the API, snapshot and library process lists repeating every system process once per GPU reader on hosts with several readers (for example NVIDIA plus AMD). One system process pass is now shared, and rows are unique per (PID, device) (#505).
- Fixed the Linux TUI command column showing `[name]` instead of the full command line for processes it discovered (#504).
- Hardware detection reads the PCI inventory once from `/sys/bus/pci/devices` and runs `lspci -nn` only when sysfs is unreadable, instead of spawning `lspci` up to eight times at startup; Apple Silicon no longer runs `system_profiler SPPCIDataType` to look for an NVIDIA GPU (#504).
- On Linux hosts with cpufreq, the reported base CPU frequency is now the average `scaling_cur_freq` over all counted CPUs (previously cpu0 on ARM and the `cpu MHz` average on x86) (#504).
- Fixed the `/metrics` and `snapshot --format prometheus` exposition repeating `# HELP` and `# TYPE` once per device and interleaving metric families. Each family is now written once with contiguous samples, so the body is valid for strict text-format parsers such as `promtool check metrics` and OpenMetrics consumers on hosts with several GPUs, processes, or volumes. Sample lines are unchanged; their order in the body changes (#503).
- Fixed replay playback ignoring the recorded frame spacing and the speed setting, never pausing or looping at the end of a recording, and showing the wrong elapsed time after 1024 frames or a seek (#503).
- API mode collects on the blocking pool with a fixed tick period, renders `/metrics` once per collection instead of once per scrape, and serializes each `/events` and `/snapshot` frame once per filter instead of once per client (#503).
- A hung reader no longer leaks one blocking thread per tick in `record` and `/snapshot`; the section is reported with `kind: "busy"` until the reader returns. `snapshot`, `record`, and the `/snapshot` fallback collect sections concurrently (#503).
- Replay seeks inside the cached window do no I/O, and far seeks use the recording's index frames (#503).
- Accelerator readers no longer spawn the same vendor CLI twice per tick: Neuron (`neuron-ls`), Rebellions (`rbln-stat --json`) and Furiosa (`furiosa-smi info`/`status`) share one result between the device and process passes, and the Intel xe reader on Linux walks `/proc/*/fd` once for both the VRAM fallback and the process rows (#502).
- Neuron utilization now comes from one long-lived `neuron-monitor` child read by a single thread, instead of a process and a helper thread spawned and killed every tick. The child is respawned 30 s after it exits, a record older than 15 s is reported as absent, and static sysfs identity is read once per device (#502).
- Failures are no longer retried every tick: a missing `rbln-stat` is looked up once per 60 s, the NVIDIA `nvidia-smi` fallback backs off from 30 s up to 300 s after a failed spawn, the Jetson `nvidia-smi` process query and failed Tenstorrent chip detection wait 60 s, and the TPU metrics server is connected to at most once per 10 s while no workload runs (#502).
- Jetson memory is read from `/proc/meminfo` instead of spawning `tegrastats --once` every tick, which could block a tick for the full command timeout (#502).
- NVIDIA: device UUID and name come from the cached static info, the GPM support probe runs once per device, and `vgpu_host_mode()` / `mig_mode()` are not probed again on a GPU that answered `NotSupported`. A GPU reporting MIG disabled is still probed every tick (#502).
- Fixed Furiosa devices staying hidden for the rest of the process when the first `furiosa-smi info` call failed (#502).

- Raised the minimum supported Rust version to 1.97, matching the Launchpad PPA toolchain (#468).
- Updated sysinfo from 0.39.6 to 0.40.4 and russh from 0.64.1 to 0.65.2, with semver-compatible lockfile refreshes; sysinfo's new default `gpu` feature stays disabled (#470, #494).

### Library API

- Removed `all_smi::device::is_running_in_container` and `all_smi::device::get_container_pid_namespace` (also under `all_smi::device::platform_detection`), which nothing in all-smi used (#504).
- Removed the unused `all_smi::device::container_utils` module (`is_running_in_container`, `is_containerized_process`, `get_container_pid_mapping`, `get_self_pid_mapping`, `map_host_to_container_pid`, `find_host_pid_from_container_pid`, `build_pid_mapping_cache`, `format_process_name_with_container_info`) and `all_smi::device::process_utils` module (`get_system_process_info`) (#504).
- Removed the unused `all_smi::utils::refresh_global_processes` (#504).
- `all_smi::device::process_list::get_all_processes` no longer returns rows for userland threads (#504).
- `all_smi::device::readers::nvidia_vgpu::collect_vgpu_info` and `nvidia_mig::collect_mig_info` take a second argument, `&nvidia_hardware::NotSupportedSkipSet`, holding the device indices that already answered `NotSupported`; pass a fresh `NotSupportedSkipSet::new()` to probe every device (#502).
- Renamed `all_smi::ui::activity_panel::CollapseStrategy::PECluster` to `TierCluster` (#474).
- Added the public `CpuInfo::cpu_tiers` and `CoreUtilization::tier_index` fields; struct literals must set them or use `..CpuInfo::test_default()`. Serde defaults keep older snapshots and remote payloads readable (#474, #482).
- Added `GpuReader::includes_system_processes`, defaulting to `false` (#505).

[Full changelog](https://github.com/lablup/all-smi/compare/v0.28.1...v0.29.0)

## v0.28.1 (2026/10/20)

- Added Rebellions REBEL100 (`RBLN-CR13`) support, verified against captures from a 4-card KMD 3.5.1 node; README, the man page, and API.md list the REBEL line (#466).
- Added per-process NPU utilization for Rebellions from each context's `util_info`, shown in the local TUI GPU% column (#466).
- Fixed the `model` label of `all_smi_rebellions_device_info` reading `ATOM` for every real card; it now follows the SKU (`ATOM`, `ATOM-Plus`, `ATOM-Max`, `REBEL100`, `REBEL`) (#466).
- Fixed the `rebellions.driver` doctor check never finding a driver version; it now reports `KMD_version` from `rbln-stat --json` (#466).
- Fixed Intel Gaudi detection on hosts whose `/dev/accel` nodes belong to another vendor, such as Rebellions REBEL; every accel node is checked and only Habana's vendor ID counts (#465).
- Fixed the `gaudi.devices` doctor check counting non-Habana accel nodes as Habana nodes (#465).
- Updated russh to 0.64.1 and the luwen crates to 0.10.0, with a semver-compatible lockfile refresh (#467).

[Full changelog](https://github.com/lablup/all-smi/compare/v0.28.0...v0.28.1)

## v0.28.0 (2026/10/17)

- Added NVIDIA RTX Spark N1X support: the integrated NPU folds into the SoC GPU row as a DLA gauge and sparkline instead of a separate device, and the GPU reports unified memory over NVLink-C2C without PCIe gauges (#460).
- Added native Windows ARM64 (`aarch64-pc-windows-msvc`) builds and the `all-smi-windows-aarch64` release asset; building on this target requires LLVM with clang on `PATH` (#460).
- Added `all_smi_dla_power_watts`, `all_smi_dla_frequency_mhz`, `all_smi_dla_clock_memory_mhz`, `all_smi_dla_temperature_celsius`, and `all_smi_dla_max_frequency_mhz` Prometheus gauges, read back by remote view (#460, #462).
- Added the N1X DLA engine clock, read from D3DKMT node performance data because NVML reports it as unsupported (#462).
- Added Windows ARM CPU detection with P/E core mapping and a registry processor-name fallback (#460).
- Added Windows thermal-zone performance counters as a last-resort CPU temperature source (#460).
- Filled WDDM per-process GPU memory from PDH samples when NVML reports it unavailable (#460).
- Fixed constant false 100% Intel Xe GPU utilization when a GT idle-residency counter never advances (#458).
- Fixed the nvidia-smi CSV fallback dividing power draw by 1000 (#460).
- Stopped retrying the `root\WMI` temperature query every poll after an access-denied error (#460).
- Added a `windows-11-arm` CI job and a Windows ARM64 release build (#460).
- Signed the Windows ARM64 release binary on the x86_64 signing runner, so `all-smi-windows-aarch64` ships Authenticode-signed (#464).

[Full changelog](https://github.com/lablup/all-smi/compare/v0.27.1...v0.28.0)

## v0.27.1 (2026/10/11)

- Added display-only `--hide-storage` to local and view modes, including replay, without changing storage collection or exports (#455).
- Fixed false remote host disconnection notices by sharing endpoint identities across collection and tabs (#454).
- Fixed remote energy and configured cost displays using chassis power and separate viewer-session totals for each endpoint (#454).
- Bounded remote HTTP response buffering to limit memory use (#454).
- Cancelled timed-out remote fetch tasks instead of leaving them running (#454).
- Prevented parser truncation from splitting UTF-8 characters (#454).
- Fixed Launchpad builds by compiling the AMD companion before packaging it (#450).
- Added CI guards for AMD companion build rules (#450).
- Added CI documentation-link checks and checker regression tests (#452).
- Reorganized operator documentation into focused guides (#452).
- Removed the Homebrew tap requirement after inclusion in homebrew-core (aaf1b36).
- Updated Rust dependencies, including clap 4.6.7, russh 0.63.3, uuid 1.26.1, and thiserror 2.0.21 (f987501).

[Full changelog](https://github.com/lablup/all-smi/compare/v0.27.0...v0.27.1)

## Release history
- **v0.29.1 (2026/11/24):** Stopped publishing the Intel macOS archive so releases no longer wait on the self-hosted Intel Mac, moved CI to the Node 24 releases of GitHub Actions, and brings the 0.29 changes to Homebrew.
- **v0.29.0 (2026/11/23):** Added N-tier CPU topology and wider Linux CPU temperature sensor coverage, fixed AMD MI300X metrics and ROCm 7.x detection, and cut per-tick collection, API and TUI overhead across platforms.
- **v0.28.1 (2026/10/20):** Added Rebellions REBEL100 support with per-process NPU utilization and SKU-based model labels, stopped reporting non-Habana accel nodes as Intel Gaudi, and updated russh and luwen.
- **v0.28.0 (2026/10/17):** Added NVIDIA RTX Spark N1X and native Windows ARM64 support with DLA metrics on the GPU row, read the N1X DLA engine clock, and fixed false 100% Intel Xe utilization from frozen GT idle counters.
- **v0.27.1 (2026/10/11):** Added storage hiding, fixed remote host tabs and energy accounting, hardened HTTP collection, repaired Launchpad AMD packaging, refreshed documentation, and updated dependencies.
- **v0.27.0 (2026/10/02):** Added AWS Neuron (Trainium and Inferentia) monitoring, cut per-tick collection cost on macOS and Linux, fixed Apple Silicon and Rebellions power over-counts, and stopped all_smi_gpu_info label churn
- **v0.26.3 (2026/09/01):** Fixed local TUI wrapping and duplicated metrics in narrow terminals, accepted comma-separated `--hosts` lists with startup validation, corrected user-facing CLI and doc text, and updated Rust dependencies including nvml-wrapper 0.13
- **v0.26.2 (2026/08/25):** Moved Linux AMD monitoring into a runtime-loaded companion, fixed unavailable Windows GPU metrics and Level Zero memory provenance, and made native Windows tests and lint checks pass.
- **v0.26.1 (2026/08/24):** Fixed macOS release signing and recovery, added early validation for Homebrew credentials and native Windows checks, and refreshed Rust dependencies
- **v0.26.0 (2026/08/24):** Run API mode as a supervised service on Linux, macOS, and Windows, add Windows GPU metrics via DXGI, PDH, and AMD ADL, and compile the Intel Level Zero backend into every Linux and Windows build
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
