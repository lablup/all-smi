// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::OnceLock;

#[cfg(target_os = "linux")]
use crate::device::common::constants::google_tpu::is_libtpu_available;
use crate::device::common::execute_command_default;

mod pci;
pub(crate) use pci::{PciDevice, any_pci_device, vendor};

// Platform detection results are immutable for the lifetime of the process:
// hardware doesn't appear or disappear at runtime. Cache each detection call
// in a process-global OnceLock so expensive probes (e.g. `system_profiler
// SPPCIDataType` on macOS, `lspci` on Linux, `nvidia-smi -L` everywhere) run
// at most once, instead of being re-executed on every view refresh cycle.

pub fn has_nvidia() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_nvidia)
}

fn detect_nvidia() -> bool {
    // No NVIDIA driver exists for Apple Silicon, so there is nothing to
    // probe. `system_profiler SPPCIDataType` takes 0.5-2 s, and the TUI's
    // notification pass calls this while holding its state lock (issue #504).
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return false;
    }

    // On Intel Macs, use system_profiler to check for NVIDIA devices
    if std::env::consts::OS == "macos" {
        // First check system_profiler for NVIDIA PCI devices
        if let Ok(output) = execute_command_default("system_profiler", &["SPPCIDataType"])
            && output.status == 0
        {
            // Look for NVIDIA in the output - could be in Type field or device name
            if output.stdout.contains("NVIDIA") {
                return true;
            }
        }

        // Fallback to nvidia-smi check
        if let Ok(output) = execute_command_default("nvidia-smi", &["-L"])
            && output.status == 0
        {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            return output
                .stdout
                .lines()
                .any(|line| line.trim().starts_with("GPU"));
        }
        return false;
    }

    // On Windows, check if nvidia-smi is available and can list GPUs
    if std::env::consts::OS == "windows" {
        // Try nvidia-smi first (most reliable on Windows)
        if let Ok(output) = execute_command_default("nvidia-smi", &["-L"])
            && output.status == 0
        {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            let has_gpu = output.stdout.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("GPU") && trimmed.contains(":")
            });
            if has_gpu {
                return true;
            }
        }

        // Try NVML directly via the nvml-wrapper crate (will be attempted in reader)
        // If nvidia-smi fails, we can still try NVML initialization
        return false;
    }

    // On Linux, first look for NVIDIA VGA/3D controllers on the PCI bus
    if any_pci_device(pci_is_nvidia_gpu) {
        return true;
    }

    // Fallback: Check if nvidia-smi can actually list GPUs
    if let Ok(output) = execute_command_default("nvidia-smi", &["-L"]) {
        // Check both exit status and output content
        if output.status == 0 {
            // nvidia-smi -L outputs lines like "GPU 0: NVIDIA GeForce..."
            // Make sure we have actual GPU lines, not just an empty output
            let has_gpu = output.stdout.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("GPU") && trimmed.contains(":")
            });
            if has_gpu {
                return true;
            }
        }

        // Also check stderr for "No devices were found" message
        if output.stderr.contains("No devices were found")
            || output.stderr.contains("Failed to initialize NVML")
        {
            return false;
        }
    }
    false
}

/// Check whether at least one AMD GPU is present.
///
/// Compiled on every glibc Linux build. Detection uses PCI/sysfs only and does
/// not load the optional AMD companion plugin, so downstream feature choices
/// cannot remove the hardware signal.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn has_amd() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_amd)
}

/// Check whether at least one Intel **client** GPU (Arc / Iris / Xe /
/// integrated graphics) is present.
///
/// Linux: delegates to [`crate::device::readers::intel_gpu_linux::has_intel_client_gpu`]
/// which walks `/sys/class/drm/card*` and falls back to the shared PCI
/// inventory.
///
/// Windows: delegates to
/// [`crate::device::readers::intel_gpu_windows::has_intel_gpu_windows`]
/// which uses a WMI query against `Win32_VideoController`.
///
/// On other platforms (macOS, BSD) Intel client GPUs are not in scope —
/// returns `false` unconditionally so the rest of the detection
/// machinery short-circuits. Result is cached in a `OnceLock` like
/// every other `has_*` detector since hardware doesn't change at
/// runtime.
pub fn has_intel_gpu() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_intel_gpu)
}

fn detect_intel_gpu() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::device::readers::intel_gpu_linux::has_intel_client_gpu()
    }
    #[cfg(target_os = "windows")]
    {
        crate::device::readers::intel_gpu_windows::has_intel_gpu_windows()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn detect_amd() -> bool {
    // On Linux, check for AMD GPUs
    if std::env::consts::OS == "linux" {
        // Check the PCI bus for AMD devices (Vendor ID 1002)
        if any_pci_device(pci_is_amd) {
            return true;
        }

        // Fallback: check /sys/class/drm
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let path = entry.path().join("device/vendor");
                if let Ok(vendor) = std::fs::read_to_string(path)
                    && vendor.trim() == "0x1002"
                {
                    return true;
                }
            }
        }
    }
    false
}

pub fn is_jetson() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_jetson)
}

fn detect_jetson() -> bool {
    if let Ok(compatible) = std::fs::read_to_string("/proc/device-tree/compatible") {
        return compatible.contains("tegra");
    }
    false
}

pub fn is_apple_silicon() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_apple_silicon)
}

fn detect_apple_silicon() -> bool {
    // Only check on macOS
    if std::env::consts::OS != "macos" {
        return false;
    }

    // An `aarch64-apple-darwin` binary can only ever run on Apple Silicon, so
    // answer from the compile-time target rather than from an external command
    // that a restricted PATH could hide. Without this, an unresolvable `uname`
    // would report Apple Silicon hardware as an Intel Mac and hand it the Intel
    // readers.
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return true;
    }

    // An x86_64 binary running under Rosetta 2 sees `uname -m` == "x86_64" even
    // though the machine is Apple Silicon, so ask the kernel whether this
    // process is translated before trusting the machine string. The probe is
    // compiled out on aarch64 builds, which can only ever run natively.
    if is_translated_process() {
        return true;
    }

    // A missing or unexecutable `uname` used to abort the whole process here.
    // Treat the failure as "not Apple Silicon" instead: every caller of this
    // function already has a working non-Apple-Silicon path, so degrading is
    // strictly better than panicking.
    match execute_command_default("uname", &["-m"]) {
        Ok(output) => output.stdout.trim() == "arm64",
        Err(_) => false,
    }
}

/// Report whether the current process runs under Rosetta 2 translation.
///
/// `sysctl.proc_translated` returns 1 for translated processes and 0 for
/// native ones. The key does not exist at all on Intel Macs, where sysctl
/// exits non-zero; that is treated as "not translated".
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn is_translated_process() -> bool {
    matches!(
        execute_command_default("sysctl", &["-n", "sysctl.proc_translated"]),
        Ok(output) if output.status == 0 && output.stdout.trim() == "1"
    )
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn is_translated_process() -> bool {
    false
}

/// Report whether this is an Intel (x86_64) Mac.
///
/// Complement of [`is_apple_silicon`] on macOS. Defined only on macOS because
/// every caller sits behind a `target_os = "macos"` gate, and an always-false
/// stub elsewhere would be dead code under the `-D warnings` build.
#[cfg(target_os = "macos")]
pub fn is_intel_mac() -> bool {
    !is_apple_silicon()
}

pub fn has_furiosa() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_furiosa)
}

fn detect_furiosa() -> bool {
    // Check if devices are visible under the /sys/class/rngd_mgmt directory
    let rngd_mgmt_path = std::path::Path::new("/sys/class/rngd_mgmt");
    if !rngd_mgmt_path.exists() {
        return false;
    }

    // Check if /sys/class/rngd_mgmt/rngd!npu0mgmt exists
    let npu0_mgmt_path = rngd_mgmt_path.join("rngd!npu0mgmt");
    if !npu0_mgmt_path.exists() {
        return false;
    }

    // Check if the content of platform_type is FuriosaAI
    let platform_type_path = npu0_mgmt_path.join("platform_type");
    if let Ok(platform_type) = std::fs::read_to_string(platform_type_path)
        && platform_type.trim() == "FuriosaAI"
    {
        return true;
    }

    false
}

#[cfg(target_os = "linux")]
pub fn has_tenstorrent() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_tenstorrent)
}

#[cfg(target_os = "linux")]
fn detect_tenstorrent() -> bool {
    // First check if device directory exists
    if std::path::Path::new("/dev/tenstorrent").exists() {
        return true;
    }

    // Then the PCI bus (Tenstorrent's vendor ID is 1e52)
    if any_pci_device(|device| device.vendor == vendor::TENSTORRENT) {
        return true;
    }

    // Last resort: check if tt-smi can actually list devices
    if let Ok(output) = execute_command_default("tt-smi", &["-s", "--snapshot_no_tty"])
        && output.status == 0
    {
        // Check if output contains device_info
        return output.stdout.contains("device_info");
    }

    false
}

/// Check whether at least one AWS Neuron device (Trainium / Inferentia)
/// is present.
///
/// Device IDs follow the AWS Neuron driver and AWS AI chips operator:
/// Inferentia1 occupies the 7064-7067 range, with later Inferentia and
/// Trainium generations using 7164, 7264, and 7364.
#[cfg(target_os = "linux")]
const NEURON_PCI_DEVICE_IDS: &[u16] = &[0x7064, 0x7065, 0x7066, 0x7067, 0x7164, 0x7264, 0x7364];

#[cfg(target_os = "linux")]
pub fn has_neuron() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_neuron)
}

#[cfg(target_os = "linux")]
fn detect_neuron() -> bool {
    // The driver creates /dev/neuron0 for the first device. Presence is
    // enough to identify the hardware even when the current user still
    // needs membership in the device node's owning group.
    if std::path::Path::new("/dev/neuron0").exists() {
        return true;
    }

    // Fall back to PCI IDs: the Neuron tools live under
    // /opt/aws/neuron/bin, which is on PATH only via the DLAMI profile
    // scripts, so probing a CLI here would report a false negative under
    // sudo, systemd, or a container entrypoint.
    any_pci_device(pci_is_neuron)
}

#[cfg(target_os = "linux")]
fn pci_is_neuron(device: &PciDevice) -> bool {
    device.vendor == vendor::AMAZON && NEURON_PCI_DEVICE_IDS.contains(&device.device)
}

pub fn has_rebellions() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_rebellions)
}

/// Is this PCI device a Rebellions card?
///
/// Rebellions' vendor ID is `1eff`, confirmed against a live ATOM Plus node:
/// `lspci -nn` prints
/// `Processing accelerators [1200]: Rebellions Inc. RBLN-CA22 (PF) [1eff:1220]`
/// and every PCI alias in the `rebellions` KMD is `v00001EFF`. The code once
/// looked for `1f3f`, which belongs to no Rebellions device. The ID does not
/// depend on how current the host's `pci.ids` is, so no name check is needed.
fn pci_is_rebellions(device: &PciDevice) -> bool {
    device.vendor == vendor::REBELLIONS
}

fn detect_rebellions() -> bool {
    // First check if device files exist (rbln0, rbln1, etc.)
    if std::path::Path::new("/dev/rbln0").exists() {
        return true;
    }

    // Then the PCI bus
    if any_pci_device(pci_is_rebellions) {
        return true;
    }

    // Last resort: check if rbln-stat or rbln-smi can actually list devices
    for cmd in &[
        "rbln-stat",
        "/usr/local/bin/rbln-stat",
        "/usr/bin/rbln-stat",
        "rbln-smi",
        "/usr/local/bin/rbln-smi",
        "/usr/bin/rbln-smi",
    ] {
        if let Ok(output) = execute_command_default(cmd, &["-j"])
            && output.status == 0
        {
            // Check if output contains device information
            if output.stdout.contains("\"devices\"") && output.stdout.contains("\"uuid\"") {
                return true;
            }
        }
    }

    false
}

/// Check if Google TPU devices are present
/// Uses only file system and environment variable checks to avoid process spawning.
/// IMPORTANT: No external commands are executed to prevent process accumulation.
#[cfg(target_os = "linux")]
pub fn has_google_tpu() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_google_tpu)
}

#[cfg(target_os = "linux")]
fn detect_google_tpu() -> bool {
    // Method 1: Check if /dev/accel* devices exist with Google vendor ID
    // This works for on-premise TPU nodes and some TPU versions
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("accel")
            {
                // Check sysfs for Google vendor ID (0x1ae0)
                let sysfs_path = format!("/sys/class/accel/{name}/device/vendor");
                if let Ok(vendor) = std::fs::read_to_string(&sysfs_path)
                    && vendor.trim() == "0x1ae0"
                {
                    return true;
                }
            }
        }
    }

    // Method 2: Check for TPU VM environment variables
    // TPU VMs (like v6e) set these environment variables
    if std::env::var("TPU_NAME").is_ok()
        || std::env::var("TPU_CHIPS_PER_HOST_BOUNDS").is_ok()
        || std::env::var("CLOUD_TPU_TASK_ID").is_ok()
        || std::env::var("TPU_ACCELERATOR_TYPE").is_ok()
        || std::env::var("TPU_WORKER_ID").is_ok()
        || std::env::var("TPU_WORKER_HOSTNAMES").is_ok()
    {
        return true;
    }

    // Method 3: Check libtpu availability combined with TPU indicators
    if is_libtpu_available() {
        // Check for PJRT TPU plugin indicators
        if let Ok(pjrt_names) = std::env::var("PJRT_DEVICE")
            && pjrt_names.to_lowercase().contains("tpu")
        {
            return true;
        }

        // If on GCE (Google Compute Engine), libtpu likely means TPU
        if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name")
            && product.to_lowercase().contains("google")
        {
            return true;
        }
    }

    false
}

pub fn has_gaudi() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_gaudi)
}

/// Habana Labs' PCI vendor ID as sysfs prints it.
const HABANA_SYSFS_VENDOR_ID: &str = "0x1da3";

/// Is an accel node with this sysfs `vendor` a Gaudi?
///
/// `/dev/accel/accel*` is the kernel's generic compute-accelerator class, not
/// a Gaudi path: Google TPUs and Rebellions REBEL cards register there too.
/// Treating every non-Google node as Gaudi made a REBEL100 host report Intel
/// Gaudi, so only Habana's own vendor ID counts.
fn accel_vendor_is_habana(vendor: &str) -> bool {
    vendor.trim().eq_ignore_ascii_case(HABANA_SYSFS_VENDOR_ID)
}

/// Sysfs class directory of the kernel's compute-accelerator nodes.
pub(crate) const ACCEL_SYSFS_CLASS_DIR: &str = "/sys/class/accel";

/// Number of Habana device nodes (`accel<N>`) under an accel class directory,
/// normally [`ACCEL_SYSFS_CLASS_DIR`].
///
/// Every node is checked, since another accelerator may own accel0, and a
/// node whose vendor cannot be read does not count. Habana's control nodes
/// (`accel_controlD<N>`) are skipped so each card counts once. Detection and
/// the `gaudi.devices` doctor check both use this, so they cannot disagree.
pub(crate) fn habana_accel_node_count(class_dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(class_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("accel"))
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("device/vendor"))
                .is_ok_and(|vendor| accel_vendor_is_habana(&vendor))
        })
        .count()
}

fn detect_gaudi() -> bool {
    // Intel Gaudi uses /dev/accel/accel* device files, but so do other
    // accelerators, so only nodes with Habana's vendor ID count.
    if habana_accel_node_count(std::path::Path::new(ACCEL_SYSFS_CLASS_DIR)) > 0 {
        return true;
    }

    // Also check /dev/hl* device files (older naming convention)
    if std::path::Path::new("/dev/hl0").exists() {
        return true;
    }

    // Check for hl-smi command availability
    const PATHS: &[&str] = &[
        "/usr/bin/hl-smi",
        "/usr/local/bin/hl-smi",
        "/opt/habanalabs/bin/hl-smi",
    ];

    for path in PATHS {
        if std::path::Path::new(path).exists() {
            return true;
        }
    }

    // On Linux, look for Habana Labs' vendor ID (1da3) on the PCI bus. The
    // ID is what `lspci` resolves to "Habana Labs", so no name check is
    // needed.
    if std::env::consts::OS == "linux" && any_pci_device(|device| device.vendor == vendor::HABANA) {
        return true;
    }

    // Last resort: check if hl-smi can actually list devices
    if let Ok(output) = execute_command_default("hl-smi", &["-L"])
        && output.status == 0
    {
        // Check if output contains device listing
        return !output.stdout.is_empty();
    }

    false
}

/// An NVIDIA VGA or 3D controller (the two classes the `lspci` text check
/// accepted).
fn pci_is_nvidia_gpu(device: &PciDevice) -> bool {
    device.vendor == vendor::NVIDIA && device.is_vga_or_3d()
}

/// Any AMD (ATI) PCI function.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn pci_is_amd(device: &PciDevice) -> bool {
    device.vendor == vendor::AMD
}

/// An Intel display controller (VGA, XGA, 3D or other display class); an
/// Intel NIC or chipset function does not count.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pci_is_intel_gpu(device: &PciDevice) -> bool {
    device.vendor == vendor::INTEL && device.is_display()
}

pub fn get_os_type() -> &'static str {
    std::env::consts::OS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `lspci -nn` line from a live 8x RBLN-CA22 (ATOM Plus) node.
    const REBELLIONS_LSPCI_LINE: &str = "03:00.0 Processing accelerators [1200]: Rebellions Inc. RBLN-CA22 (PF) [1eff:1220] (rev 03)";

    /// Regression: any accel node that was not a Google TPU was taken for a
    /// Gaudi, so a REBEL100 host reported Intel Gaudi next to Rebellions.
    #[test]
    fn only_habana_accel_nodes_are_gaudi() {
        assert!(accel_vendor_is_habana("0x1da3\n"));
        assert!(accel_vendor_is_habana("0x1DA3"));
        // Rebellions REBEL and Google TPU accel nodes are not Gaudi.
        assert!(!accel_vendor_is_habana("0x1eff\n"));
        assert!(!accel_vendor_is_habana("0x1ae0\n"));
        assert!(!accel_vendor_is_habana(""));
    }

    /// Regression: only accel0 was checked, and an unreadable vendor counted
    /// as Gaudi. A Habana card behind a REBEL card on accel0 must still be
    /// found, and nothing else under the accel class may count.
    #[test]
    fn habana_accel_nodes_are_counted_across_the_class() {
        let class = tempfile::tempdir().expect("tempdir");
        let node = |name: &str, vendor: Option<&str>| {
            let device = class.path().join(name).join("device");
            std::fs::create_dir_all(&device).expect("node dir");
            if let Some(vendor) = vendor {
                std::fs::write(device.join("vendor"), vendor).expect("vendor file");
            }
        };

        node("accel0", Some("0x1eff\n")); // Rebellions REBEL
        node("accel1", Some("0x1ae0\n")); // Google TPU
        node("accel2", None); // vendor unreadable
        assert_eq!(habana_accel_node_count(class.path()), 0);

        node("accel3", Some("0x1da3\n"));
        node("accel_controlD3", Some("0x1da3\n")); // same card's control node
        node("accelerator", Some("0x1da3\n"));
        assert_eq!(habana_accel_node_count(class.path()), 1);

        assert_eq!(habana_accel_node_count(&class.path().join("missing")), 0);
    }

    /// Whether `pred` matches any device in verbatim `lspci -nn` output.
    fn lspci_matches(pred: impl Fn(&PciDevice) -> bool, output: &str) -> bool {
        pci::parse_lspci_nn(output).iter().any(pred)
    }

    /// Regression: detection looked for vendor ID `1f3f`, which is not
    /// Rebellions. It only appeared to work because the same line also carries
    /// the vendor *name*, so a host with an old `pci.ids` — where the line
    /// reads `Device [1eff:1220]` — would not have been detected at all.
    #[test]
    fn rebellions_is_detected_by_its_real_pci_vendor_id() {
        assert!(lspci_matches(pci_is_rebellions, REBELLIONS_LSPCI_LINE));

        // Same device on a host whose pci.ids cannot name the vendor.
        let unnamed = REBELLIONS_LSPCI_LINE.replace("Rebellions Inc. RBLN-CA22 (PF)", "Device");
        assert!(
            lspci_matches(pci_is_rebellions, &unnamed),
            "vendor ID alone must be enough: {unnamed}"
        );

        // The wrong constant must not be what matches.
        assert!(!lspci_matches(
            pci_is_rebellions,
            "03:00.0 Processing accelerators [1200]: Device [1f3f:1220] (rev 03)"
        ));

        // Unrelated accelerators are not Rebellions.
        assert!(!lspci_matches(
            pci_is_rebellions,
            "03:00.0 Processing accelerators [1200]: Habana Labs Ltd. HL-325L [1da3:1060]"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn neuron_detection_accepts_every_supported_pci_generation() {
        for id in NEURON_PCI_DEVICE_IDS {
            let line = format!(
                "00:1e.0 Processing accelerators [1200]: Amazon.com, Inc. Device [1d0f:{id:04x}]"
            );
            assert!(
                lspci_matches(pci_is_neuron, &line),
                "missing AWS Neuron PCI ID {id:04x}"
            );
        }
        assert!(lspci_matches(
            pci_is_neuron,
            "00:1e.0 Processing accelerators [1200]: Amazon Device [1D0F:7164]"
        ));
        assert!(!lspci_matches(
            pci_is_neuron,
            "00:05.0 Ethernet controller [0200]: Amazon.com, Inc. Elastic Network Adapter [1d0f:ec20]"
        ));
        assert!(!lspci_matches(
            pci_is_neuron,
            "00:04.0 Non-Volatile memory controller [0108]: Amazon.com, Inc. NVMe EBS Controller [1d0f:8061]"
        ));
    }

    /// Issue #504: every detector reads the one shared inventory, so each
    /// vendor's verbatim `lspci -nn` line must match its own detector and no
    /// other.
    #[cfg(target_os = "linux")]
    #[test]
    fn each_vendor_fixture_matches_only_its_own_detector() {
        type Detector = (&'static str, fn(&PciDevice) -> bool);
        fn tenstorrent(device: &PciDevice) -> bool {
            device.vendor == vendor::TENSTORRENT
        }
        fn gaudi(device: &PciDevice) -> bool {
            device.vendor == vendor::HABANA
        }
        let detectors: [Detector; 7] = [
            ("nvidia", pci_is_nvidia_gpu),
            ("amd", pci_is_amd_for_tests),
            ("intel", pci_is_intel_gpu),
            ("gaudi", gaudi),
            ("neuron", pci_is_neuron),
            ("rebellions", pci_is_rebellions),
            ("tenstorrent", tenstorrent),
        ];
        let fixtures = [
            (
                "nvidia",
                "41:00.0 3D controller [0302]: NVIDIA Corporation GH100 [H100 PCIe] [10de:2331] (rev a1)",
            ),
            (
                "nvidia",
                "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA102 [GeForce RTX 3080] [10de:2206] (rev a1)",
            ),
            (
                "amd",
                "03:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Navi 21 [Radeon RX 6800/6800 XT / 6900 XT] [1002:73bf] (rev c1)",
            ),
            (
                "amd",
                "c1:00.0 Processing accelerators [1200]: Advanced Micro Devices, Inc. [AMD/ATI] Aldebaran/MI200 [Instinct MI250X/MI250] [1002:740c] (rev 01)",
            ),
            (
                "intel",
                "03:00.0 VGA compatible controller [0300]: Intel Corporation DG2 [Arc A770] [8086:56a0] (rev 08)",
            ),
            (
                "intel",
                "00:02.0 Display controller [0380]: Intel Corporation Meteor Lake-P [Intel Arc Graphics] [8086:7d55] (rev 08)",
            ),
            (
                "gaudi",
                "19:00.0 Processing accelerators [1200]: Habana Labs Ltd. HL-2000 AI Training Accelerator [Gaudi] [1da3:1000] (rev 01)",
            ),
            (
                "neuron",
                "00:1e.0 System peripheral [0880]: Amazon.com, Inc. Device [1d0f:7164]",
            ),
            ("rebellions", REBELLIONS_LSPCI_LINE),
            (
                "tenstorrent",
                "01:00.0 Processing accelerators [1200]: Tenstorrent Inc. Wormhole [1e52:401e] (rev 01)",
            ),
        ];
        for (owner, line) in fixtures {
            let devices = pci::parse_lspci_nn(line);
            assert_eq!(devices.len(), 1, "unparsed fixture: {line}");
            for (name, detector) in detectors {
                assert_eq!(
                    detector(&devices[0]),
                    name == owner,
                    "{name} on the {owner} fixture: {line}"
                );
            }
        }

        // Non-accelerator functions of the same vendors match nothing.
        for line in [
            "00:1f.6 Ethernet controller [0200]: Intel Corporation Ethernet Connection [8086:15fc] (rev 20)",
            "41:00.1 Audio device [0403]: NVIDIA Corporation GH100 HD Audio [10de:22ba] (rev a1)",
            "00:00.0 Host bridge [0600]: Advanced Micro Devices, Inc. [AMD] Starship/Matisse Root Complex [1022:1480]",
        ] {
            let devices = pci::parse_lspci_nn(line);
            for (name, detector) in detectors {
                assert!(!detector(&devices[0]), "{name} matched {line}");
            }
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    fn pci_is_amd_for_tests(device: &PciDevice) -> bool {
        pci_is_amd(device)
    }

    #[cfg(all(target_os = "linux", target_env = "musl"))]
    fn pci_is_amd_for_tests(device: &PciDevice) -> bool {
        device.vendor == vendor::AMD
    }

    /// Issue #504: no NVIDIA driver exists for Apple Silicon, so detection
    /// answers without spawning `system_profiler` or `nvidia-smi`.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn apple_silicon_has_no_nvidia() {
        assert!(!detect_nvidia());
    }

    /// Architecture detection must never abort the process (it used to
    /// `.expect()` on the `uname` probe) and must be stable across calls
    /// because the result is cached in a `OnceLock`.
    #[test]
    fn apple_silicon_detection_is_total_and_stable() {
        let first = is_apple_silicon();
        assert_eq!(first, is_apple_silicon());

        if std::env::consts::OS != "macos" {
            assert!(!first, "non-macOS hosts are never Apple Silicon");
        }
    }

    /// An `aarch64-apple-darwin` binary can only ever run on Apple Silicon, so
    /// detection must answer from the compile-time target and never depend on
    /// resolving `uname`. Failing open to `false` here would hand Apple Silicon
    /// hardware the Intel readers.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn aarch64_macos_build_reports_apple_silicon() {
        assert!(is_apple_silicon());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn intel_mac_is_the_macos_complement_of_apple_silicon() {
        assert_eq!(is_intel_mac(), !is_apple_silicon());
    }
}

/// Aggregated hardware-detection snapshot.
///
/// Extracted from the individual detector functions so `all-smi doctor`
/// (issue #188) and `reader_factory` share a single call-site for
/// "what hardware is present on this host?". Calling
/// [`introspection::snapshot`] once at startup is cheaper than calling
/// each detector individually because every detector is already cached in
/// a `OnceLock` — the snapshot is just a struct wrapper with read-only
/// accessors.
pub mod introspection {
    /// Summary of hardware detected on this host. Every field is the
    /// result of the corresponding `has_*` / `is_*` detector in the
    /// parent module. Fields are `bool`s so consumers can match on
    /// structural shape rather than re-running detection.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct PlatformSnapshot {
        pub os: &'static str,
        pub nvidia: bool,
        pub jetson: bool,
        /// `true` on glibc Linux targets when AMD hardware is detected;
        /// always `false` on musl and non-Linux builds. Plugin availability is
        /// reported separately by `all-smi doctor`.
        pub amd: bool,
        pub apple_silicon: bool,
        pub gaudi: bool,
        pub google_tpu: bool,
        pub tenstorrent: bool,
        pub rebellions: bool,
        pub furiosa: bool,
        /// `true` when an AWS Neuron device (Trainium / Inferentia) is
        /// detected. Linux-only; always `false` elsewhere.
        pub neuron: bool,
        /// `true` when an Intel **client** GPU (Arc / Iris / Xe /
        /// integrated graphics) is detected. Reported on both Linux
        /// (i915 / xe drivers) and Windows (WMI). Distinct from
        /// `gaudi`, which is the Intel datacenter HPU.
        pub intel_gpu: bool,
    }

    /// Produce a fresh [`PlatformSnapshot`] from the cached detectors.
    pub fn snapshot() -> PlatformSnapshot {
        PlatformSnapshot {
            os: super::get_os_type(),
            nvidia: super::has_nvidia(),
            jetson: super::is_jetson(),
            amd: detect_amd(),
            apple_silicon: super::is_apple_silicon(),
            gaudi: super::has_gaudi(),
            google_tpu: detect_google_tpu(),
            tenstorrent: detect_tenstorrent(),
            rebellions: super::has_rebellions(),
            furiosa: super::has_furiosa(),
            neuron: detect_neuron(),
            intel_gpu: super::has_intel_gpu(),
        }
    }

    // These two arms must stay exact complements: the positive one is
    // compiled only where `super::has_amd` exists, the negative one
    // everywhere else. Any drift produces either a duplicate definition or a
    // missing `detect_amd`.
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    fn detect_amd() -> bool {
        super::has_amd()
    }

    #[cfg(not(all(target_os = "linux", not(target_env = "musl"))))]
    fn detect_amd() -> bool {
        false
    }

    #[cfg(target_os = "linux")]
    fn detect_google_tpu() -> bool {
        super::has_google_tpu()
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_google_tpu() -> bool {
        false
    }

    #[cfg(target_os = "linux")]
    fn detect_tenstorrent() -> bool {
        super::has_tenstorrent()
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_tenstorrent() -> bool {
        false
    }

    // Same exact-complement rule as `detect_amd` above.
    #[cfg(target_os = "linux")]
    fn detect_neuron() -> bool {
        super::has_neuron()
    }

    #[cfg(not(target_os = "linux"))]
    fn detect_neuron() -> bool {
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn snapshot_os_matches_consts() {
            let snap = snapshot();
            assert_eq!(snap.os, std::env::consts::OS);
        }

        #[test]
        fn snapshot_is_default_friendly() {
            // Ensure `PlatformSnapshot` is constructible via `Default::default()`
            // for mock/test scenarios.
            let empty = PlatformSnapshot::default();
            assert_eq!(empty.os, "");
            assert!(!empty.nvidia);
            // Intel client GPU detection (issue #244) must default to
            // `false` so unconfigured mock/test scenarios don't
            // accidentally claim Intel hardware.
            assert!(!empty.intel_gpu);
        }
    }
}
