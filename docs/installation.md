# Installation and hardware requirements

[Home](../README.md) · [Documentation](../README.md#documentation)

## Option 1: Install via Homebrew (macOS/Linux)

The easiest way to install all-smi on macOS and Linux is through Homebrew:

```bash
brew install all-smi
```

## Option 2: Install via Ubuntu PPA

For Ubuntu users, all-smi is available through the official PPA:

```bash
# Add the PPA repository
sudo add-apt-repository ppa:lablup/backend-ai
sudo apt update

# Install all-smi
sudo apt install all-smi
```

The PPA provides automatic updates and is maintained for Ubuntu 22.04 (Jammy), 24.04 (Noble), and 26.04 (Resolute).

## Option 3: Install via Debian Package

For a compatible Debian-based distribution, download the `.deb` package built for your Ubuntu release and architecture from the [releases page](https://github.com/lablup/all-smi/releases):

```bash
# Replace VERSION, OS, and ARCH with the exact release asset name
wget https://github.com/lablup/all-smi/releases/download/vVERSION/all-smi_VERSION_OS_ARCH.deb
# Example: all-smi_VERSION_ubuntu24.04.noble_amd64.deb

# Install the package
sudo dpkg -i all-smi_VERSION_OS_ARCH.deb

# If there are dependency issues, fix them with:
sudo apt-get install -f
```

## Option 4: Download Pre-built Binary

Download the latest release from the [GitHub releases page](https://github.com/lablup/all-smi/releases):

1. Go to https://github.com/lablup/all-smi/releases
2. Download the appropriate binary for your platform
3. Extract the archive and place the binary in your `$PATH`. On glibc Linux, keep `liball_smi_amd.so` beside the binary or install it under `/usr/local/lib/all-smi`; the companion provides AMD monitoring without making the main binary depend on libdrm.

> Release binaries are signed: macOS archives are notarized (so Gatekeeper does not block them as coming from an unidentified developer) and Windows binaries are Authenticode code-signed.

## Option 5: Install from Cargo

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

`cargo install` installs Cargo binary targets only, so it does not install the Linux AMD companion library. The resulting binary still starts on every host and `all-smi doctor --only amd` reports the plugin as unavailable; use the release archive, Homebrew, PPA/Debian package, or the source-build instructions below when Linux AMD monitoring is required.

## Option 6: Build from Source

See [Building from Source](../DEVELOPERS.md#building-from-source) in the developer documentation.

## A note on containers

all-smi does not ship a container image, and container deployment is not a supported install path. Use one of the six options above, and see [Running as a service](services.md) for supervising API mode under systemd, launchd, or the Windows Service Control Manager.

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
- **Local-mode privilege gate:** On glibc Linux, `all-smi` / `all-smi local` checks for root when an AMD GPU is detected, even if device permissions would otherwise suffice. Run `sudo all-smi local` for this path.
- **Driver:** A working `amdgpu` kernel driver and userspace DRM libraries are required. A full ROCm installation is not required for the companion reader.
- **Build Requirements:**
  - AMD GPU support is available in **glibc builds only** (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`)
  - **Not available in musl builds** (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) due to library compatibility
- **Runtime Companion Library:** Linux AMD monitoring is loaded at runtime from `liball_smi_amd.so`, the only artifact that links `libdrm.so.2` and `libdrm_amdgpu.so.1`. The main binary has no libdrm dependency and starts on every host; without the companion or AMD's userspace DRM libraries (`libdrm2 libdrm-amdgpu1` on Debian/Ubuntu, `libdrm` on RHEL/Fedora) it reports no AMD GPU, and `all-smi doctor --only amd` names what is missing. Release archives, Homebrew, and the Debian/PPA packages install the companion; `cargo install` does not
- **The `amd` cargo feature:** an accepted no-op kept for downstream manifest compatibility. It adds no dependency, and `--no-default-features` no longer removes AMD detection. Building the companion from source needs the libdrm development files; see [DEVELOPERS.md](../DEVELOPERS.md#the-amd-companion-library)
- **Device access for API/snapshot/library use:** Membership in `video` and `render` can provide access to `/dev/dri` without root. This does not bypass the current local-mode privilege gate:
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
- **Level Zero Metrics:** The Level Zero backend is built into every Linux release. Install the Intel oneAPI Level Zero runtime so `libze_loader.so.1` is present at runtime and it activates on its own. When available, Sysman adds per-engine activity (including the XMX `COMPUTE_SINGLE` class), energy-counter power, temperature, memory, and frequency on top of the sysfs baseline; when absent, all-smi silently falls back to the sysfs baseline

### Windows
- **Privileges:** Basic GPU and CPU monitoring can run without Administrator rights; some sensor providers require elevation or a separately running sensor service.
- **Intel client GPUs:** Arc / Iris Xe / Xe metrics come from three stacked layers. WMI names the card, DXGI and PDH supply the 64-bit memory capacity, memory in use, and utilization, and Intel Level Zero (Sysman) supplies temperature, power, frequency, and fan. The Level Zero layer is built into every Windows release, no feature flag required, and activates when `ze_loader.dll` (installed with the Intel graphics driver) is present; without it the first two layers still report. `detail["Metrics Source"]` names the layers that contributed, and `detail["Note"]` names any metric none of them could supply
- **CPU Temperature Limitations:**
  - Standard Windows WMI thermal zones (MSAcpi_ThermalZoneTemperature) are not available on all systems
  - The application uses a fallback chain to try multiple temperature sources:
    1. ACPI Thermal Zones (standard WMI)
    2. AMD Ryzen Master SDK (AMD CPUs - requires AMD drivers or Ryzen Master)
    3. Intel WMI (Intel CPUs - if chipset drivers support it)
    4. LibreHardwareMonitor WMI (any CPU - if [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor) is running)
  - If temperature is not available, it will be shown as "N/A" without error messages
  - For best temperature monitoring on Windows, install and run LibreHardwareMonitor in the background

## Accelerator backends on Linux

| Hardware | Runtime source / requirement |
|---|---|
| NVIDIA Jetson | Tegra platform support and `tegrastats`; some readings need elevated permissions |
| Intel Gaudi | Habana driver and `hl-smi` |
| Google Cloud TPU | TPU host/runtime; availability varies by TPU generation and metric source |
| Tenstorrent | Tenstorrent driver and device access; Wormhole and Blackhole supported, Grayskull skipped |
| Rebellions | Rebellions driver and `rbln-stat` (`rbln-smi` fallback) |
| Furiosa RNGD | Furiosa driver and `furiosa-smi` |
| AWS Neuron | Neuron driver/sysfs, `neuron-ls`, and optionally `neuron-monitor` |

AWS Neuron reports one row per NeuronCore. Utilization is best-effort while a Neuron runtime is attached; unavailable temperature, power, and clock readings are not measurements of zero.

Use [diagnostics](troubleshooting.md) to check which readers are available on your host. Metrics depend on hardware, drivers, permissions, and collection mode; support for a device does not imply every metric is available.
