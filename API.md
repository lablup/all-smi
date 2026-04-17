# API Metrics Reference

`all-smi` provides comprehensive hardware metrics in Prometheus format through its API mode. This document details all available metrics across different hardware platforms.

## Starting API Mode

```bash
# Start API server on TCP port
all-smi api --port 9090

# Custom update interval (default: 3 seconds)
all-smi api --port 9090 --interval 5

# Include process information
all-smi api --port 9090 --processes
```

Metrics are available at `http://localhost:9090/metrics`

### Unix Domain Socket Support (Unix Only)

For local IPC scenarios, API mode supports Unix Domain Sockets:

```bash
# Use default socket path
all-smi api --socket
# Linux: /var/run/all-smi.sock (or /tmp/all-smi.sock)
# macOS: /tmp/all-smi.sock

# Use custom socket path
all-smi api --socket /custom/path/all-smi.sock

# TCP and Unix socket simultaneously
all-smi api --port 9090 --socket

# Unix socket only (disable TCP)
all-smi api --port 0 --socket
```

Access metrics via Unix socket:
```bash
curl --unix-socket /tmp/all-smi.sock http://localhost/metrics
```

```python
# Python example
import requests_unixsocket
session = requests_unixsocket.Session()
r = session.get('http+unix://%2Ftmp%2Fall-smi.sock/metrics')
```

**Security**: Socket permissions are set to `0600` (owner-only access).

## Available Metrics

### GPU Metrics (All Platforms)

| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | GPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | GPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | GPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | GPU temperature            | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | GPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | GPU frequency              | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | GPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |

### Unified AI Acceleration Library Labels

The `all_smi_gpu_info` metric includes standardized labels for AI acceleration libraries across all GPU/accelerator platforms. These unified labels allow platform-agnostic queries and dashboards:

| Label         | Description                              | Example Values                    |
|---------------|------------------------------------------|-----------------------------------|
| `lib_name`    | Name of the AI acceleration library      | `CUDA`, `ROCm`, `Metal`          |
| `lib_version` | Version of the AI acceleration library   | `13.0`, `7.0.2`, `Metal 3`       |

#### Platform-Specific Library Mappings

| Platform          | lib_name | lib_version Source | Platform-Specific Label |
|-------------------|----------|-------------------|-------------------------|
| NVIDIA GPU        | `CUDA`   | CUDA version      | `cuda_version`         |
| AMD GPU           | `ROCm`   | ROCm version      | `rocm_version`         |
| NVIDIA Jetson     | `CUDA`   | CUDA version      | `cuda_version`         |
| Apple Silicon     | `Metal`  | Metal version     | N/A                    |

**Note**: Platform-specific labels (e.g., `cuda_version`, `rocm_version`) are maintained for backward compatibility with existing queries and dashboards.

#### Example PromQL Queries

```promql
# Count devices by AI library type
count by (lib_name) (all_smi_gpu_info)

# Get all CUDA devices with version 12 or higher
all_smi_gpu_info{lib_name="CUDA", lib_version=~"1[2-9].*|[2-9][0-9].*"}

# Alert on outdated ROCm versions (< 7.0)
all_smi_gpu_info{lib_name="ROCm", lib_version!~"[7-9].*"} == 1

# Cross-platform library distribution
sum by (lib_name, lib_version) (all_smi_gpu_info)

# Find all devices using Metal (Apple Silicon)
all_smi_gpu_info{lib_name="Metal"}

# Monitor library version consistency across cluster
count by (lib_name, lib_version) (all_smi_gpu_info) > 1
```

### NVIDIA GPU Specific Metrics

| Metric                                                    | Description                                                        | Unit    | Labels                               |
|-----------------------------------------------------------|--------------------------------------------------------------------|---------|--------------------------------------|
| `all_smi_gpu_pcie_gen_current`                            | Current PCIe generation                                            | -       | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_pcie_width_current`                          | Current PCIe link width                                            | -       | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_performance_state`                           | GPU performance state (P0=0 … P15=15; omitted when not reported)  | -       | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_temperature_threshold_slowdown_celsius`      | Slowdown temperature threshold                                     | celsius | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_temperature_threshold_shutdown_celsius`      | Shutdown temperature threshold                                     | celsius | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_temperature_threshold_max_operating_celsius` | Maximum operating temperature threshold                            | celsius | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_temperature_threshold_acoustic_celsius`      | Acoustic (fan-noise) temperature threshold                         | celsius | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_clock_graphics_max_mhz`                      | Maximum graphics clock                                             | MHz     | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_clock_memory_max_mhz`                        | Maximum memory clock                                               | MHz     | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_power_limit_current_watts`                   | Current power limit                                                | watts   | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |
| `all_smi_gpu_power_limit_max_watts`                       | Maximum power limit                                                | watts   | `gpu`, `instance`, `gpu_uuid`, `gpu_index`   |

**Notes:**
- Threshold metrics (`temperature_threshold_*`) and `performance_state` are NVIDIA-only. Each metric is emitted only when the driver exposes the value; hosts where the driver does not report a given threshold simply omit that metric line.
- `performance_state` maps NVML `PerformanceState` variants: P0 (maximum performance) = 0 through P15 = 15. The `Unknown` sentinel is suppressed (`None`) rather than emitted.
- The acoustic threshold is available on newer drivers and some GPU SKUs; older drivers leave it absent.

### NVIDIA Hardware Details Metrics

Extended NVIDIA hardware detail metrics (NUMA topology, GSP firmware, NvLink topology, and GPU Performance Monitoring). All metrics in this group share the same four-label base set as other NVIDIA-specific metrics (`gpu`, `instance`, `gpu_uuid`, `gpu_index`) and are omitted entirely for non-NVIDIA devices, on older drivers that do not expose the underlying NVML APIs, or when the field value is unavailable.

| Metric                                    | Description                                                                                                      | Unit  | Labels                                                               |
|-------------------------------------------|------------------------------------------------------------------------------------------------------------------|-------|----------------------------------------------------------------------|
| `all_smi_gpu_numa_node_id`                | NUMA node the GPU is attached to; omitted when the host has no NUMA topology or the driver does not report one   | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`                                   |
| `all_smi_gpu_gsp_firmware_mode`           | GSP firmware mode: `0`=disabled, `1`=enabled, `2`=default; omitted on pre-R525 drivers or non-datacenter SKUs  | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`                                   |
| `all_smi_gpu_gsp_firmware_version_info`   | Info-style metric (value always 1) carrying the GSP firmware version string in a `version` label                | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`, `version`                        |
| `all_smi_nvlink_remote_device_type`       | Info-style metric (value always 1) per active NvLink; classification in `remote_type` label                     | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`, `link_index`, `remote_type`      |
| `all_smi_gpu_sm_occupancy`                | GPM-reported SM occupancy fraction (0.0–1.0); omitted on pre-Hopper GPUs or when GPM has not yet sampled        | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`                                   |
| `all_smi_gpu_memory_bandwidth_utilization`| GPM-reported DRAM bandwidth utilization fraction (0.0–1.0); omitted when GPM is unsupported or unsampled        | gauge | `gpu`, `instance`, `gpu_uuid`, `gpu_index`                                   |

**Label values for `all_smi_nvlink_remote_device_type`:**

| Label        | Values                                   | Description                                       |
|--------------|------------------------------------------|---------------------------------------------------|
| `link_index` | `"0"`, `"1"`, …                         | NvLink port index on the GPU                      |
| `remote_type`| `"gpu"`, `"switch"`, `"ibmnpu"`, `"unknown"` | Classification of the remote endpoint        |

**Notes:**
- `all_smi_gpu_numa_node_id`: NVML reports `-1` for GPUs without a NUMA attachment; this value is canonicalised to `None` and the metric is omitted rather than emitting a negative number.
- `all_smi_gpu_gsp_firmware_version_info`: the `version` label carries a string such as `"550.54.15"`. Because the version is static for the lifetime of the driver, it is cached after the first successful NVML call.
- `all_smi_nvlink_remote_device_type`: one metric row is emitted per active NvLink. A GPU with no active links produces no rows for this metric family.
- `all_smi_gpu_sm_occupancy` and `all_smi_gpu_memory_bandwidth_utilization`: GPM requires a two-sample handshake before values are available. Until the handshake completes the exporter holds `None` for both fields and emits nothing, preventing spurious zero readings. These metrics are currently plumbing only — values are populated on Hopper and later hardware when the GPM handshake succeeds.
- To simulate the full set of extended hardware detail metrics (including the thermal thresholds and `performance_state` listed in the NVIDIA GPU Specific Metrics table above) in development/testing without a modern NVIDIA driver, set `ALL_SMI_MOCK_HARDWARE_DETAILS=1` when running with the `mock` feature. When unset, the mock omits these families to simulate an older driver that does not expose the underlying NVML APIs.

### NVIDIA vGPU Metrics

NVIDIA vGPU metrics are emitted only on hosts with vGPU SR-IOV enabled. Non-vGPU hosts produce no output for these metric families.

#### Host-Level vGPU Metrics

| Metric                         | Description                                               | Unit  | Labels                                                                  |
|--------------------------------|-----------------------------------------------------------|-------|-------------------------------------------------------------------------|
| `all_smi_vgpu_host_mode`       | vGPU host mode (0=NonSriov, 1=Sriov, 2=Disabled)         | gauge | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `host_mode`        |
| `all_smi_vgpu_scheduler_state` | vGPU scheduler ARR mode (0=unsupported, 1=off, 2=ARR)    | gauge | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `arr_supported`    |
| `all_smi_vgpu_scheduler_policy`| vGPU scheduler policy id                                 | gauge | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`                     |

#### Per-vGPU Instance Metrics

| Metric                           | Description                                             | Unit    | Labels                                                                                              |
|----------------------------------|---------------------------------------------------------|---------|-----------------------------------------------------------------------------------------------------|
| `all_smi_vgpu_utilization`       | Per-vGPU GPU utilization percentage (0-100)             | percent | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `vgpu_id`, `vgpu_uuid`, `vgpu_type`, `vgpu_vm_id` |
| `all_smi_vgpu_memory_utilization`| Per-vGPU memory bandwidth utilization percentage (0-100)| percent | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `vgpu_id`, `vgpu_uuid`, `vgpu_type`, `vgpu_vm_id` |
| `all_smi_vgpu_memory_used_bytes` | Per-vGPU framebuffer memory used                        | bytes   | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `vgpu_id`, `vgpu_uuid`, `vgpu_type`, `vgpu_vm_id` |
| `all_smi_vgpu_memory_total_bytes`| Per-vGPU framebuffer memory budget                      | bytes   | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `vgpu_id`, `vgpu_uuid`, `vgpu_type`, `vgpu_vm_id` |
| `all_smi_vgpu_active`            | Per-vGPU liveness (1=accounting PID active, 0=idle)     | gauge   | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `vgpu_id`, `vgpu_uuid`, `vgpu_type`, `vgpu_vm_id` |

**Notes:**
- `vgpu_utilization` is only emitted when NVML accounting data is available for the vGPU instance.
- `vgpu_memory_utilization` is only emitted when NVML reports memory bandwidth usage.
- The `vgpu_vm_id` label carries the owning VM identifier so remote scrapers can reconstruct the same VM column shown in the TUI.
- To simulate vGPU responses in development/testing without real vGPU hardware, set `ALL_SMI_MOCK_VGPU=1` when running with the `mock` feature.

### NVIDIA MIG Metrics

NVIDIA MIG (Multi-Instance GPU) metrics are emitted only on hosts where at least one GPU has MIG mode enabled or has active MIG instances. Non-MIG hosts produce no output for these metric families.

#### Per-GPU MIG Mode

| Metric                  | Description                                         | Unit  | Labels                                                          |
|-------------------------|-----------------------------------------------------|-------|-----------------------------------------------------------------|
| `all_smi_gpu_mig_mode`  | MIG mode per parent GPU (1=enabled, 0=disabled)     | gauge | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`             |

#### Per-MIG-Instance Metrics

| Metric                                    | Description                                           | Unit    | Labels                                                                                                                    |
|-------------------------------------------|-------------------------------------------------------|---------|---------------------------------------------------------------------------------------------------------------------------|
| `all_smi_mig_instance_utilization_gpu`    | Per-MIG-instance GPU SM utilization percentage (0-100)| percent | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `mig_instance`, `mig_uuid`, `mig_profile`, `gpu_instance_id`, `compute_instance_id` |
| `all_smi_mig_instance_utilization_memory` | Per-MIG-instance memory bandwidth utilization (0-100) | percent | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `mig_instance`, `mig_uuid`, `mig_profile`, `gpu_instance_id`, `compute_instance_id` |
| `all_smi_mig_instance_memory_used_bytes`  | Per-MIG-instance framebuffer memory used              | bytes   | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `mig_instance`, `mig_uuid`, `mig_profile`, `gpu_instance_id`, `compute_instance_id` |
| `all_smi_mig_instance_memory_total_bytes` | Per-MIG-instance framebuffer memory total carve-out   | bytes   | `gpu_index`, `gpu_uuid`, `gpu`, `instance`, `host`, `mig_instance`, `mig_uuid`, `mig_profile`, `gpu_instance_id`, `compute_instance_id` |

**Label descriptions for per-instance metrics:**

| Label                | Description                                                                 |
|----------------------|-----------------------------------------------------------------------------|
| `gpu_index`          | Physical GPU index on the host                                              |
| `gpu_uuid`           | UUID of the parent physical GPU                                             |
| `gpu`                | Name of the parent GPU model                                                |
| `instance`           | Prometheus instance label (hostname or host:port)                           |
| `host`               | Hostname of the server                                                      |
| `mig_instance`       | MIG instance ordinal index within the parent GPU                            |
| `mig_uuid`           | UUID of the MIG compute instance (assigned by NVML)                         |
| `mig_profile`        | MIG profile name (e.g. `1g.5gb`, `3g.20gb`, `7g.40gb`)                    |
| `gpu_instance_id`    | NVML GPU instance ID; empty string when not reported by the driver          |
| `compute_instance_id`| NVML compute instance ID; empty string when not reported by the driver      |

**Notes:**
- `all_smi_mig_instance_utilization_gpu` and `all_smi_mig_instance_utilization_memory` are emitted only when NVML reports utilization for that instance; the metric line is absent when the value is unavailable.
- `gpu_instance_id` and `compute_instance_id` are emitted as empty string labels when NVML cannot report them, preserving round-trip fidelity in the remote parser.
- MIG instances appear as nested rows under their parent GPU in the TUI, matched by `gpu_uuid` with a hostname+GPU-name fallback.
- `all_smi_gpu_mig_mode` is emitted for every GPU that supports MIG (enabled or disabled), so consumers can detect mode transitions even when no instances are active.
- To simulate MIG responses in development/testing without MIG hardware, set `ALL_SMI_MOCK_MIG=1` when running with the `mock` feature.

### NVIDIA Jetson Specific Metrics

| Metric                    | Description                                 | Unit    | Labels                  |
|---------------------------|---------------------------------------------|---------|-------------------------|
| `all_smi_dla_utilization` | DLA (Deep Learning Accelerator) utilization | percent | `gpu_index`, `gpu_name` |

### AMD GPU Specific Metrics

AMD GPUs (Radeon and Instinct series) provide comprehensive monitoring through ROCm and the DRM subsystem:

| Metric                        | Description                              | Unit    | Labels                                      |
|-------------------------------|------------------------------------------|---------|---------------------------------------------|
| `all_smi_gpu_fan_speed_rpm`   | GPU fan speed                            | RPM     | `gpu_index`, `gpu_name`                     |
| `all_smi_amd_rocm_version`    | AMD ROCm version installed               | info    | `instance`, `version`                       |
| `all_smi_gpu_memory_gtt_bytes`| GTT (GPU Translation Table) memory usage | bytes   | `gpu_index`, `gpu_name`                     |
| `all_smi_gpu_memory_vram_bytes`| VRAM (Video RAM) usage                  | bytes   | `gpu_index`, `gpu_name`                     |

**Additional Details Available** (in `all_smi_gpu_info` labels):
- **Driver Version**: AMDGPU kernel driver version (e.g., "30.10.1")
- **ROCm Version**: ROCm software stack version (e.g., "7.0.2")
- **PCIe Information**: Current link generation and width, max GPU/system link capabilities
- **VBIOS**: Version and date information
- **Power Management**: Current, minimum, and maximum power cap values
- **ASIC Information**: Device ID, revision ID, ASIC name
- **Memory Clock**: Current memory clock frequency

**Process Tracking**:
- AMD GPU process detection uses `fdinfo` from `/proc/<pid>/fdinfo/` for accurate memory tracking
- Tracks both VRAM and GTT memory usage per process
- Available with `--processes` flag in API mode

**Platform Requirements**:
- Requires ROCm drivers and `libamdgpu_top` library
- Requires sudo access to `/dev/dri` devices or user in `video`/`render` groups
- Only available in glibc builds (not musl static builds)

### Apple Silicon GPU Specific Metrics

| Metric                          | Description            | Unit  | Labels                           |
|---------------------------------|------------------------|-------|----------------------------------|
| `all_smi_ane_utilization`       | ANE utilization        | mW    | `gpu_index`, `gpu_name`          |
| `all_smi_ane_power_watts`       | ANE power consumption  | watts | `gpu_index`, `gpu_name`          |
| `all_smi_thermal_pressure_info` | Thermal pressure level | info  | `gpu_index`, `gpu_name`, `level` |

Note: For Apple Silicon (M1/M2/M3/M4), `gpu_temperature_celsius` is not available; thermal pressure level is provided instead.

### Tenstorrent NPU Metrics

#### Basic NPU Metrics
| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | NPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | NPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | NPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | NPU ASIC temperature       | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | NPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | NPU AI clock frequency     | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | NPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |
| `all_smi_npu_firmware_info`           | NPU firmware version       | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `firmware` |

#### Tenstorrent-Specific Metrics
| Metric                                          | Description                        | Unit    | Labels                                                    |
|-------------------------------------------------|------------------------------------|---------|-----------------------------------------------------------|
| `all_smi_tenstorrent_board_info`                | Board and architecture information | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `board_type`, `board_id`, `architecture` |
| `all_smi_tenstorrent_collection_method_info`    | Data collection method used        | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `method`             |
| **Firmware Versions**                           |                                    |         |                                                           |
| `all_smi_tenstorrent_arc_firmware_info`         | ARC firmware version               | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tenstorrent_eth_firmware_info`         | Ethernet firmware version          | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tenstorrent_ddr_firmware_info`         | DDR firmware version               | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tenstorrent_spibootrom_firmware_info`  | SPI Boot ROM firmware version      | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tenstorrent_firmware_date_info`        | Firmware build date                | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `date`               |
| **Temperature Sensors**                         |                                    |         |                                                           |
| `all_smi_tenstorrent_asic_temperature_celsius`  | ASIC temperature                   | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_vreg_temperature_celsius`  | Voltage regulator temperature      | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_inlet_temperature_celsius` | Inlet temperature                  | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_outlet1_temperature_celsius`| Outlet 1 temperature              | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_outlet2_temperature_celsius`| Outlet 2 temperature              | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| **Clock Frequencies**                           |                                    |         |                                                           |
| `all_smi_tenstorrent_aiclk_mhz`                | AI clock frequency                 | MHz     | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_axiclk_mhz`               | AXI clock frequency                | MHz     | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_arcclk_mhz`               | ARC clock frequency                | MHz     | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| **Power and Electrical**                        |                                    |         |                                                           |
| `all_smi_tenstorrent_voltage_volts`            | Core voltage                       | volts   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_current_amperes`          | Current draw                       | amperes | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_power_raw_watts`          | Raw power consumption              | watts   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_tdp_limit_watts`          | TDP limit                          | watts   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_tdc_limit_amperes`        | TDC limit                          | amperes | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| **Status and Health**                           |                                    |         |                                                           |
| `all_smi_tenstorrent_heartbeat`                | Device heartbeat counter           | counter | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_arc0_health`              | ARC0 health counter                | counter | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_arc3_health`              | ARC3 health counter                | counter | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_faults`                   | Fault register value               | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_throttler`                | Throttler state register           | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_pcie_status_info`         | PCIe status register               | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `status`             |
| `all_smi_tenstorrent_eth_status_info`          | Ethernet status register           | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `port`, `status`     |
| `all_smi_tenstorrent_ddr_status`               | DDR status register                | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| **Fan Metrics**                                 |                                    |         |                                                           |
| `all_smi_tenstorrent_fan_speed_percent`        | Fan speed percentage               | percent | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_fan_rpm`                  | Fan speed in RPM                   | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| **PCIe Information**                            |                                    |         |                                                           |
| `all_smi_tenstorrent_pcie_generation`          | PCIe generation                    | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_pcie_width`               | PCIe link width                    | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tenstorrent_pcie_address_info`        | PCIe address                       | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `address`            |
| `all_smi_tenstorrent_pcie_device_info`         | PCIe device identification         | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `vendor_id`, `device_id` |
| **DRAM Information**                            |                                    |         |                                                           |
| `all_smi_tenstorrent_dram_info`                | DRAM configuration                 | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `speed`              |

Note: Tenstorrent NPUs use the same basic metric names as GPUs for compatibility with existing monitoring infrastructure. Additional Tenstorrent-specific metrics provide detailed hardware monitoring capabilities.

### Rebellions NPU Metrics

#### Basic NPU Metrics
| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | NPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | NPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | NPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | NPU temperature            | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | NPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | NPU clock frequency        | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | NPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |

#### Rebellions-Specific Metrics
| Metric                                    | Description                          | Unit  | Labels                                                               |
|-------------------------------------------|--------------------------------------|-------|----------------------------------------------------------------------|
| `all_smi_rebellions_device_info`          | Device model and variant information | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `model`, `variant`              |
| `all_smi_rebellions_firmware_info`        | NPU firmware version                 | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `firmware_version`              |
| `all_smi_rebellions_kmd_info`             | Kernel Mode Driver version           | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `kmd_version`                   |
| `all_smi_rebellions_device_status`        | Device operational status            | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_rebellions_performance_state`    | NPU performance state (P0-P15)       | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_rebellions_pcie_generation`      | PCIe generation (Gen4)               | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_rebellions_pcie_width`           | PCIe link width (x16)                | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_rebellions_memory_bandwidth_gbps`| Memory bandwidth capacity            | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_rebellions_compute_tops`         | Compute capacity in TOPS             | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |

Note: Rebellions NPUs support ATOM, ATOM+, and ATOM Max variants with varying compute and memory capabilities. All variants use PCIe Gen4 x16 interface.

### Furiosa NPU Metrics

#### Basic NPU Metrics
| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | NPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | NPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | NPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | NPU temperature            | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | NPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | NPU clock frequency        | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | NPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |

#### Furiosa-Specific Metrics
| Metric                                      | Description                            | Unit    | Labels                                                               |
|---------------------------------------------|----------------------------------------|---------|----------------------------------------------------------------------|
| `all_smi_furiosa_device_info`               | Device architecture and model info     | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `architecture`, `model`         |
| `all_smi_furiosa_firmware_info`             | NPU firmware version                   | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `firmware_version`              |
| `all_smi_furiosa_pert_info`                 | PERT (runtime) version                 | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `pert_version`                  |
| `all_smi_furiosa_liveness_status`           | Device liveness status                 | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_furiosa_core_count`                | Number of cores in NPU                 | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_furiosa_core_status`               | Core availability status               | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`, `core`                          |
| `all_smi_furiosa_pe_utilization`            | Processing Element utilization         | percent | `npu`, `instance`, `npu_uuid`, `npu_index`, `core`                          |
| `all_smi_furiosa_core_frequency_mhz`        | Per-core frequency                     | MHz     | `npu`, `instance`, `npu_uuid`, `npu_index`, `core`                          |
| `all_smi_furiosa_power_governor_info`       | Power governor mode                    | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `governor`                      |
| `all_smi_furiosa_error_count`               | Cumulative error count                 | counter | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_furiosa_pcie_generation`           | PCIe generation                        | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_furiosa_pcie_width`                | PCIe link width                        | gauge   | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |
| `all_smi_furiosa_memory_bandwidth_utilization` | Memory bandwidth utilization        | percent | `npu`, `instance`, `npu_uuid`, `npu_index`                                  |

Note: Furiosa NPUs use the RNGD architecture with 8 cores per NPU. Each core contains multiple Processing Elements (PEs) that handle neural network computations. The power governor supports OnDemand mode for dynamic power management.

### Intel Gaudi NPU Metrics

#### Basic NPU Metrics
| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | NPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | NPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | NPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | NPU temperature            | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | NPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | NPU clock frequency        | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | NPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |

#### Intel Gaudi-Specific Metrics
| Metric                                        | Description                              | Unit    | Labels                                                        |
|-----------------------------------------------|------------------------------------------|---------|---------------------------------------------------------------|
| `all_smi_gaudi_device_info`                   | Device model and information             | info    | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_internal_name_info`            | Internal device name (e.g., HL-325L)     | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `internal_name`          |
| `all_smi_gaudi_driver_info`                   | Habana driver version                    | info    | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`                |
| `all_smi_gaudi_aip_utilization_percent`       | AIP (AI Processor) utilization           | percent | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_memory_used_bytes`             | HBM memory used                          | bytes   | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_memory_total_bytes`            | HBM total memory                         | bytes   | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_memory_utilization_percent`    | HBM memory utilization percentage        | percent | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_power_draw_watts`              | Current power consumption                | watts   | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_power_max_watts`               | Maximum power limit                      | watts   | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_power_utilization_percent`     | Power utilization percentage             | percent | `npu`, `instance`, `npu_uuid`, `npu_index`                           |
| `all_smi_gaudi_temperature_celsius`           | AIP temperature                          | celsius | `npu`, `instance`, `npu_uuid`, `npu_index`                           |

Note: Intel Gaudi NPUs (Gaudi 1/2/3) are monitored via the `hl-smi` command-line tool running as a background process. Device names are automatically mapped from internal identifiers (e.g., HL-325L) to human-friendly names (e.g., Intel Gaudi 3 PCIe LP). The tool supports various form factors including PCIe, OAM, UBB, and HLS variants.

### Google TPU Metrics

#### Basic NPU Metrics
| Metric                                | Description                | Unit    | Labels                                    |
|---------------------------------------|----------------------------|---------|-------------------------------------------|
| `all_smi_gpu_utilization`             | TPU utilization percentage | percent | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_used_bytes`       | TPU memory used            | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_memory_total_bytes`      | TPU memory total           | bytes   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_temperature_celsius`     | TPU temperature            | celsius | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_power_consumption_watts` | TPU power consumption      | watts   | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_frequency_mhz`           | TPU clock frequency        | MHz     | `gpu_index`, `gpu_name`                   |
| `all_smi_gpu_info`                    | TPU device information     | info    | `gpu_index`, `gpu_name`, `driver_version` |

#### TPU-Specific Metrics
| Metric                                     | Description                          | Unit  | Labels                                                   |
|--------------------------------------------|--------------------------------------|-------|----------------------------------------------------------|
| `all_smi_tpu_utilization_percent`          | TPU duty cycle utilization           | percent| `npu`, `instance`, `npu_uuid`, `npu_index`                      |
| `all_smi_tpu_memory_used_bytes`            | TPU HBM memory used                  | bytes | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_memory_total_bytes`           | TPU HBM memory total                 | bytes | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_memory_utilization_percent`   | TPU HBM memory utilization percentage| percent| `npu`, `instance`, `npu_uuid`, `npu_index`                      |
| `all_smi_tpu_chip_version_info`            | TPU chip version information         | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tpu_accelerator_type_info`        | TPU accelerator type information     | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `type`               |
| `all_smi_tpu_core_count`                   | Number of TPU cores                  | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_tensorcore_count`             | Number of TensorCores per chip       | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_memory_type_info`             | TPU memory type (HBM2/HBM2e/HBM3e)    | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `type`               |
| `all_smi_tpu_runtime_version_info`         | TPU runtime/library version          | info  | `npu`, `instance`, `npu_uuid`, `npu_index`, `version`            |
| `all_smi_tpu_power_max_watts`              | TPU maximum power limit              | watts | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_queue_size`               | Number of pending HLO programs       | gauge | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_exec_mean_microseconds`   | HLO execution timing (mean)          | µs    | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_exec_p50_microseconds`    | HLO execution timing (P50)           | µs    | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_exec_p90_microseconds`    | HLO execution timing (P90)           | µs    | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_exec_p95_microseconds`    | HLO execution timing (P95)           | µs    | `npu`, `instance`, `npu_uuid`, `npu_index`                       |
| `all_smi_tpu_hlo_exec_p999_microseconds`   | HLO execution timing (P99.9)         | µs    | `npu`, `instance`, `npu_uuid`, `npu_index`                       |

Note: Google Cloud TPUs (v2-v7/Ironwood) are monitored via the `tpu-info` command-line tool running in streaming mode. Metrics include duty cycle utilization, HBM memory tracking, and chip configuration details.

### CPU Metrics (All Platforms)

| Metric                                | Description                | Unit    | Labels   |
|---------------------------------------|----------------------------|---------|----------|
| `all_smi_cpu_utilization`             | CPU utilization percentage | percent | -        |
| `all_smi_cpu_socket_count`            | Number of CPU sockets      | count   | -        |
| `all_smi_cpu_core_count`              | Total number of CPU cores  | count   | -        |
| `all_smi_cpu_thread_count`            | Total number of CPU threads| count   | -        |
| `all_smi_cpu_frequency_mhz`           | CPU frequency              | MHz     | -        |
| `all_smi_cpu_temperature_celsius`     | CPU temperature            | celsius | -        |
| `all_smi_cpu_power_consumption_watts` | CPU power consumption      | watts   | -        |
| `all_smi_cpu_socket_utilization`      | Per-socket CPU utilization | percent | `socket` |

### Apple Silicon CPU Specific Metrics

| Metric                                | Description                    | Unit    | Labels |
|---------------------------------------|--------------------------------|---------|--------|
| `all_smi_cpu_p_core_count`            | Number of performance cores    | count   | -      |
| `all_smi_cpu_e_core_count`            | Number of efficiency cores     | count   | -      |
| `all_smi_cpu_gpu_core_count`          | Number of integrated GPU cores | count   | -      |
| `all_smi_cpu_p_core_utilization`      | P-core utilization percentage  | percent | -      |
| `all_smi_cpu_e_core_utilization`      | E-core utilization percentage  | percent | -      |
| `all_smi_cpu_p_cluster_frequency_mhz` | P-cluster frequency            | MHz     | -      |
| `all_smi_cpu_e_cluster_frequency_mhz` | E-cluster frequency            | MHz     | -      |

### Memory Metrics (All Platforms)

| Metric                           | Description                   | Unit    | Labels |
|----------------------------------|-------------------------------|---------|--------|
| `all_smi_memory_total_bytes`     | Total system memory           | bytes   | -      |
| `all_smi_memory_used_bytes`      | Used system memory            | bytes   | -      |
| `all_smi_memory_available_bytes` | Available system memory       | bytes   | -      |
| `all_smi_memory_free_bytes`      | Free system memory            | bytes   | -      |
| `all_smi_memory_utilization`     | Memory utilization percentage | percent | -      |
| `all_smi_swap_total_bytes`       | Total swap space              | bytes   | -      |
| `all_smi_swap_used_bytes`        | Used swap space               | bytes   | -      |
| `all_smi_swap_free_bytes`        | Free swap space               | bytes   | -      |

### Linux-Specific Memory Metrics

| Metric                         | Description             | Unit  | Labels |
|--------------------------------|-------------------------|-------|--------|
| `all_smi_memory_buffers_bytes` | Memory used for buffers | bytes | -      |
| `all_smi_memory_cached_bytes`  | Memory used for cache   | bytes | -      |

### Storage Metrics

| Metric                         | Description          | Unit  | Labels        |
|--------------------------------|----------------------|-------|---------------|
| `all_smi_disk_total_bytes`     | Total disk space     | bytes | `mount_point` |
| `all_smi_disk_available_bytes` | Available disk space | bytes | `mount_point` |

Note: Storage metrics exclude Docker bind mounts and are filtered to show only relevant filesystems.

### Chassis/Node-Level Metrics

Chassis metrics provide visibility into system-wide power consumption, thermal conditions, and cooling status at the node level. These metrics aggregate information from CPU, GPU, ANE, and BMC sensors.

#### Common Chassis Metrics (All Platforms)

| Metric                              | Description                                    | Unit    | Labels                  |
|-------------------------------------|------------------------------------------------|---------|-------------------------|
| `all_smi_chassis_power_watts`       | Total chassis power consumption (CPU+GPU+ANE)  | watts   | `hostname`, `instance`  |

#### Apple Silicon Chassis Metrics

| Metric                                   | Description                               | Unit    | Labels                           |
|------------------------------------------|-------------------------------------------|---------|----------------------------------|
| `all_smi_chassis_thermal_pressure_info`  | Thermal pressure level                    | info    | `hostname`, `instance`, `level`  |
| `all_smi_chassis_cpu_power_watts`        | CPU power consumption                     | watts   | `hostname`, `instance`           |
| `all_smi_chassis_gpu_power_watts`        | GPU power consumption                     | watts   | `hostname`, `instance`           |
| `all_smi_chassis_ane_power_watts`        | ANE (Apple Neural Engine) power           | watts   | `hostname`, `instance`           |

#### Server Chassis Metrics (BMC-enabled Systems)

| Metric                                      | Description                      | Unit    | Labels                                     |
|---------------------------------------------|----------------------------------|---------|-------------------------------------------|
| `all_smi_chassis_inlet_temperature_celsius` | Chassis inlet temperature        | celsius | `hostname`, `instance`                    |
| `all_smi_chassis_outlet_temperature_celsius`| Chassis outlet temperature       | celsius | `hostname`, `instance`                    |
| `all_smi_chassis_fan_speed_rpm`             | Fan speed                        | RPM     | `hostname`, `instance`, `fan_id`, `fan_name` |

Note: Chassis metrics provide a unified view of node-level power consumption and thermal conditions, useful for cluster-wide capacity planning and power monitoring.

### Runtime Environment Metrics

| Metric                              | Description                                      | Unit  | Labels                                           |
|-------------------------------------|--------------------------------------------------|-------|--------------------------------------------------|
| `all_smi_runtime_environment`       | Current runtime environment (container or VM)    | gauge | `hostname`, `environment`                        |
| `all_smi_container_runtime_info`    | Container runtime environment information        | gauge | `hostname`, `runtime`, `container_id`            |
| `all_smi_kubernetes_pod_info`       | Kubernetes pod information (K8s only)            | gauge | `hostname`, `pod_name`, `namespace`              |
| `all_smi_virtualization_info`       | Virtualization environment information           | gauge | `hostname`, `vm_type`, `hypervisor`             |

Runtime environment metrics are detected at startup and provide information about the execution context:
- Container environments: Docker, Kubernetes, Podman, containerd, LXC, CRI-O, Backend.AI
- Virtualization platforms: VMware, VirtualBox, KVM, QEMU, Hyper-V, Xen, AWS EC2, Google Cloud, Azure, DigitalOcean, Parallels

### Process Metrics (When --processes Flag is Used)

| Metric                             | Description                     | Unit    | Labels                                                 |
|------------------------------------|---------------------------------|---------|--------------------------------------------------------|
| `all_smi_gpu_process_memory_bytes` | GPU memory used by process      | bytes   | `gpu_index`, `gpu_name`, `pid`, `process_name`, `user` |
| `all_smi_gpu_process_sm_util`      | Process GPU SM utilization      | percent | `gpu_index`, `gpu_name`, `pid`, `process_name`, `user` |
| `all_smi_gpu_process_mem_util`     | Process GPU memory utilization  | percent | `gpu_index`, `gpu_name`, `pid`, `process_name`, `user` |
| `all_smi_gpu_process_enc_util`     | Process GPU encoder utilization | percent | `gpu_index`, `gpu_name`, `pid`, `process_name`, `user` |
| `all_smi_gpu_process_dec_util`     | Process GPU decoder utilization | percent | `gpu_index`, `gpu_name`, `pid`, `process_name`, `user` |

## Platform Support Matrix

| Platform                     | GPU Metrics    | CPU Metrics    | Memory Metrics | Process Metrics |
|------------------------------|----------------|----------------|----------------|-----------------|
| Linux + NVIDIA               | ✓ Full         | ✓ Full         | ✓ Full         | ✓ Full          |
| Linux + Intel Gaudi          | ✓ Full         | ✓ Full         | ✓ Full         | ✗ N/A*******    |
| Linux + Tenstorrent          | ✓ Full***      | ✓ Full         | ✓ Full         | ✗ N/A****       |
| Linux + Rebellions           | ✓ Full         | ✓ Full         | ✓ Full         | ✗ N/A*****      |
| Linux + Furiosa              | ✓ Full         | ✓ Full         | ✓ Full         | ✗ N/A******     |
| Linux + Google TPU           | ✓ Full         | ✓ Full         | ✓ Full         | ✗ N/A********    |
| macOS + Apple Silicon        | ✓ Partial*     | ✓ Enhanced**   | ✓ Full         | ✓ Basic         |
| NVIDIA Jetson                | ✓ Full + DLA   | ✓ Full         | ✓ Full         | ✓ Full          |

*Apple Silicon (M1/M2/M3/M4) GPU metrics do not include temperature (thermal pressure provided instead)
**Apple Silicon (M1/M2/M3/M4) provides enhanced P-core/E-core metrics and cluster frequencies
***Tenstorrent provides extensive hardware monitoring including multiple temperature sensors, health counters, and status registers
****Tenstorrent NPUs do not expose per-process GPU usage information
*****Rebellions NPUs do not expose per-process GPU usage information
******Furiosa NPUs do not expose per-process GPU usage information
*******Intel Gaudi NPUs do not expose per-process GPU usage information via hl-smi
********Google Cloud TPUs do not expose per-process GPU usage information via tpu-info

## Example Prometheus Queries

### Basic Monitoring
```promql
# Average GPU utilization across all GPUs
avg(all_smi_gpu_utilization)

# Memory usage percentage per GPU
(all_smi_gpu_memory_used_bytes / all_smi_gpu_memory_total_bytes) * 100

# GPUs running above 80°C
all_smi_gpu_temperature_celsius > 80
```

### Power Monitoring
```promql
# Total power consumption across all GPUs
sum(all_smi_gpu_power_consumption_watts)

# Power efficiency (utilization per watt)
all_smi_gpu_utilization / all_smi_gpu_power_consumption_watts
```

### NVIDIA Thermal Thresholds and P-State
```promql
# GPUs within 5°C of the slowdown threshold (approaching throttle)
all_smi_gpu_temperature_threshold_slowdown_celsius - all_smi_gpu_temperature_celsius < 5

# GPUs within 2°C of the shutdown threshold (critical)
all_smi_gpu_temperature_threshold_shutdown_celsius - all_smi_gpu_temperature_celsius < 2

# Thermal headroom to slowdown per GPU
all_smi_gpu_temperature_threshold_slowdown_celsius - all_smi_gpu_temperature_celsius

# GPUs currently in a degraded performance state (not P0)
all_smi_gpu_performance_state > 0
```

### NVIDIA vGPU Specific
```promql
# All vGPU-enabled physical GPUs by SR-IOV host mode
all_smi_vgpu_host_mode{host_mode="Sriov"}

# vGPU instances with high utilization (> 80%)
all_smi_vgpu_utilization > 80

# vGPU framebuffer occupancy per instance
all_smi_vgpu_memory_used_bytes / all_smi_vgpu_memory_total_bytes * 100

# Count active vGPU instances per physical GPU
count by (gpu_uuid) (all_smi_vgpu_active == 1)

# GPUs using Adaptive Round Robin scheduler
all_smi_vgpu_scheduler_state == 2

# vGPU memory bandwidth saturation
all_smi_vgpu_memory_utilization > 90
```

### NVIDIA MIG Specific
```promql
# GPUs with MIG mode enabled
all_smi_gpu_mig_mode == 1

# MIG instances with high GPU SM utilization (> 80%)
all_smi_mig_instance_utilization_gpu > 80

# MIG framebuffer occupancy per instance
all_smi_mig_instance_memory_used_bytes / all_smi_mig_instance_memory_total_bytes * 100

# Count active MIG instances per parent GPU
count by (gpu_uuid) (all_smi_mig_instance_memory_total_bytes)

# MIG instances by profile type
count by (mig_profile) (all_smi_mig_instance_memory_total_bytes)

# Total memory carved out for MIG across the cluster
sum(all_smi_mig_instance_memory_total_bytes)
```

### AMD GPU Specific
```promql
# AMD GPUs with high fan speed (potential cooling issues)
all_smi_gpu_fan_speed_rpm > 3000

# VRAM utilization percentage
(all_smi_gpu_memory_vram_bytes / all_smi_gpu_memory_total_bytes) * 100

# AMD GPUs approaching power cap
all_smi_gpu_power_consumption_watts / all_smi_amd_power_cap_watts > 0.9

# Memory bandwidth usage (VRAM + GTT)
all_smi_gpu_memory_vram_bytes + all_smi_gpu_memory_gtt_bytes

# AMD GPU thermal efficiency (utilization per degree)
all_smi_gpu_utilization / all_smi_gpu_temperature_celsius
```

### Apple Silicon Specific
```promql
# P-core vs E-core utilization comparison
all_smi_cpu_p_core_utilization - all_smi_cpu_e_core_utilization

# ANE power consumption in watts
all_smi_ane_power_watts
```

### Tenstorrent NPU Specific
```promql
# NPUs with high temperature on any sensor
max by (instance) ({
  __name__=~"all_smi_tenstorrent_.*_temperature_celsius",
  instance=~"tt.*"
}) > 80

# Power efficiency by board type
all_smi_gpu_utilization / on(instance) group_left(board_type) 
  (all_smi_tenstorrent_board_info * 0 + all_smi_gpu_power_consumption_watts)

# Throttling detection
all_smi_tenstorrent_throttler > 0

# Health monitoring - ARC processors not incrementing
rate(all_smi_tenstorrent_arc0_health[5m]) == 0
```

### Rebellions NPU Specific
```promql
# NPUs in low performance state
all_smi_rebellions_performance_state > 0

# Devices with non-operational status
all_smi_rebellions_device_status != 1

# Power efficiency (TOPS per watt)
all_smi_rebellions_compute_tops / all_smi_gpu_power_consumption_watts

# Memory bandwidth saturation check
(all_smi_gpu_memory_used_bytes / all_smi_gpu_memory_total_bytes) > 0.9
```

### Furiosa NPU Specific
```promql
# NPUs with unavailable cores
all_smi_furiosa_core_status == 0

# Average PE utilization across all cores
avg by (instance) (all_smi_furiosa_pe_utilization)

# NPUs with high error rates
rate(all_smi_furiosa_error_count[5m]) > 0.1

# Power governor not in OnDemand mode
all_smi_furiosa_power_governor_info{governor!="OnDemand"}

# Memory bandwidth bottleneck detection
all_smi_furiosa_memory_bandwidth_utilization > 80
```

### Intel Gaudi NPU Specific
```promql
# NPUs with high AIP utilization
all_smi_gaudi_aip_utilization_percent > 80

# HBM memory utilization across cluster
avg by (instance) (all_smi_gaudi_memory_utilization_percent)

# NPUs approaching power limit
all_smi_gaudi_power_draw_watts / all_smi_gaudi_power_max_watts > 0.9

# Power efficiency (AIP utilization per watt)
all_smi_gaudi_aip_utilization_percent / all_smi_gaudi_power_draw_watts

# NPUs running hot (temperature > 70°C)
all_smi_gaudi_temperature_celsius > 70

# Total HBM memory usage across all Gaudi NPUs
sum(all_smi_gaudi_memory_used_bytes)

# Gaudi NPUs by device variant
count by (internal_name) (all_smi_gaudi_internal_name_info)

# Driver version consistency check
count by (version) (all_smi_gaudi_driver_info) > 1
```

### Google TPU Specific
```promql
# TPU utilization across all chips
avg(all_smi_tpu_utilization_percent)

# HBM memory utilization percentage
all_smi_tpu_memory_utilization_percent

# Count TPUs by accelerator type
count by (type) (all_smi_tpu_accelerator_type_info)

# Monitor HLO queue size
all_smi_tpu_hlo_queue_size > 5

# Alert on high HLO execution latency
all_smi_tpu_hlo_exec_p90_microseconds > 1000000
```

### Process Monitoring
```promql
# Top 5 GPU memory consumers
topk(5, all_smi_gpu_process_memory_bytes)

# Processes using more than 1GB GPU memory
all_smi_gpu_process_memory_bytes > 1073741824
```

### Chassis/Node-Level Monitoring
```promql
# Total power consumption across all nodes
sum(all_smi_chassis_power_watts)

# Nodes with high power consumption (> 3000W)
all_smi_chassis_power_watts > 3000

# Power breakdown by component (Apple Silicon)
sum by (hostname) (all_smi_chassis_cpu_power_watts)
sum by (hostname) (all_smi_chassis_gpu_power_watts)
sum by (hostname) (all_smi_chassis_ane_power_watts)

# Nodes with non-nominal thermal pressure
all_smi_chassis_thermal_pressure_info{level!="Nominal"}

# Average chassis power per node
avg(all_smi_chassis_power_watts)

# Nodes with high inlet temperature
all_smi_chassis_inlet_temperature_celsius > 35

# Delta between inlet and outlet temperature (thermal dissipation)
all_smi_chassis_outlet_temperature_celsius - all_smi_chassis_inlet_temperature_celsius

# Fan speed monitoring
avg by (hostname) (all_smi_chassis_fan_speed_rpm)
```

### Runtime Environment Monitoring
```promql
# All containers running in Kubernetes
all_smi_container_runtime_info{runtime="Kubernetes"}

# All instances running in AWS EC2
all_smi_virtualization_info{vm_type="AWS EC2"}

# Containers running in Backend.AI
all_smi_runtime_environment{environment="Backend.AI"}

# Group metrics by runtime environment
sum by (environment) (all_smi_gpu_utilization) * on(hostname) group_left(environment) all_smi_runtime_environment
```

## Integration Examples

### Grafana Dashboard
Create a comprehensive monitoring dashboard with:
- GPU utilization heatmap
- Memory usage time series
- Power consumption stacked graph
- Temperature alerts
- Process resource usage table

### AlertManager Rules
```yaml
groups:
  - name: gpu_alerts
    rules:
      - alert: HighGPUTemperature
        expr: all_smi_gpu_temperature_celsius > 85
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "GPU {{ $labels.gpu_name }} is running hot"
          
      - alert: GPUMemoryExhausted
        expr: (all_smi_gpu_memory_used_bytes / all_smi_gpu_memory_total_bytes) > 0.95
        for: 5m
        labels:
          severity: critical
          
      - alert: TenstorrentNPUFault
        expr: all_smi_tenstorrent_faults > 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Tenstorrent NPU {{ $labels.instance }} has fault condition"
          
      - alert: TenstorrentNPUThrottling
        expr: all_smi_tenstorrent_throttler > 0
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Tenstorrent NPU {{ $labels.instance }} is throttling"
          
      - alert: RebellionsNPULowPerformance
        expr: all_smi_rebellions_performance_state > 5
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "Rebellions NPU {{ $labels.instance }} stuck in low performance state P{{ $value }}"
          
      - alert: FuriosaNPUCoreFailure
        expr: all_smi_furiosa_core_status == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Furiosa NPU {{ $labels.instance }} has unavailable core {{ $labels.core }}"
          
      - alert: FuriosaNPUHighErrorRate
        expr: rate(all_smi_furiosa_error_count[5m]) > 1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Furiosa NPU {{ $labels.instance }} experiencing high error rate"

      - alert: GaudiNPUHighTemperature
        expr: all_smi_gaudi_temperature_celsius > 80
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Intel Gaudi NPU {{ $labels.instance }} is running hot at {{ $value }}°C"

      - alert: GaudiNPUPowerLimitApproaching
        expr: all_smi_gaudi_power_draw_watts / all_smi_gaudi_power_max_watts > 0.95
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Intel Gaudi NPU {{ $labels.instance }} approaching power limit"

      - alert: GaudiNPUHBMMemoryExhausted
        expr: all_smi_gaudi_memory_utilization_percent > 95
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Intel Gaudi NPU {{ $labels.instance }} HBM memory nearly exhausted"

      - alert: ChassisHighPowerConsumption
        expr: all_smi_chassis_power_watts > 3500
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Chassis {{ $labels.hostname }} power consumption is high at {{ $value }}W"

      - alert: ChassisThermalPressureElevated
        expr: all_smi_chassis_thermal_pressure_info{level!="Nominal"} == 1
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "Chassis {{ $labels.hostname }} thermal pressure elevated to {{ $labels.level }}"

      - alert: ChassisHighInletTemperature
        expr: all_smi_chassis_inlet_temperature_celsius > 40
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Chassis {{ $labels.hostname }} inlet temperature is high at {{ $value }}°C"
```

## Update Intervals

The metrics update interval can be configured:
- Default: 3 seconds
- Minimum recommended: 1 second
- Maximum recommended: 60 seconds

Higher update rates provide more real-time data but increase system load. For production monitoring, 5-10 seconds is typically sufficient.

## Notes

1. All metrics follow Prometheus naming conventions
2. Labels are used to differentiate between multiple devices
3. Info metrics (ending in `_info`) provide static metadata
4. Some metrics may not be available on all platforms
5. Process metrics require the `--processes` flag and may impact performance
6. Tenstorrent NPU metrics include comprehensive hardware monitoring data:
   - Multiple temperature sensors (ASIC, voltage regulator, inlet/outlet)
   - Detailed firmware versions and health counters
   - Power limits (TDP/TDC) and throttling information
   - PCIe and DDR status registers for diagnostics
7. Tenstorrent utilization is calculated based on power consumption as a proxy metric
8. Rebellions NPU metrics include:
   - Performance state monitoring (P0-P15) for power management
   - Device status and KMD version tracking
   - Support for ATOM, ATOM+, and ATOM Max variants
   - PCIe Gen4 x16 interface metrics
9. Furiosa NPU metrics include:
   - Per-core PE utilization monitoring
   - Core availability status tracking
   - Power governor mode information
   - Error counting and liveness monitoring
   - RNGD architecture with 8 cores per NPU
10. Intel Gaudi NPU metrics include:
    - AIP (AI Processor) utilization monitoring
    - HBM memory usage and utilization tracking (up to 128GB per device)
    - Power consumption with configurable power limits (up to 850W)
    - Temperature monitoring
    - Automatic device name mapping (HL-325L → Intel Gaudi 3 PCIe LP)
    - Support for Gaudi 1/2/3 across PCIe, OAM, UBB, and HLS form factors
    - Background process monitoring via hl-smi with circular buffer
11. Chassis/Node-level metrics include:
    - Total chassis power consumption aggregating CPU, GPU, and ANE power
    - Thermal pressure monitoring (Apple Silicon)
    - Individual power component breakdown (CPU, GPU, ANE)
    - Inlet/outlet temperature monitoring (BMC-enabled servers)
    - Fan speed monitoring with per-fan granularity
12. NVIDIA vGPU metrics include:
    - Host-level SR-IOV mode and scheduler configuration per physical GPU
    - Per-vGPU instance utilization, framebuffer memory, and liveness
    - VM identifier label (`vgpu_vm_id`) for correlating instances with virtual machines
    - Completely silent on non-vGPU hosts — no empty metric families are emitted
    - Requires NVIDIA vGPU-capable hardware and the GRID/vGPU driver stack
    - Set `ALL_SMI_MOCK_VGPU=1` (with `--features mock` build) to simulate vGPU data for development
13. NVIDIA MIG metrics include:
    - Per-GPU MIG mode status (`all_smi_gpu_mig_mode`); emitted for every MIG-capable GPU regardless of whether instances are active
    - Per-instance SM utilization, memory bandwidth utilization, and framebuffer used/total bytes
    - MIG profile name (`mig_profile`, e.g. `1g.5gb`, `3g.20gb`) and NVML instance IDs for correlating with `nvidia-smi mig`
    - TUI renders MIG instances as nested rows under each parent GPU, matched by UUID with hostname+GPU-name fallback
    - Completely silent on non-MIG hosts — no empty metric families are emitted
    - Set `ALL_SMI_MOCK_MIG=1` (with `--features mock` build) to simulate MIG data for development
14. NVIDIA extended hardware detail metrics include:
    - NUMA topology (`all_smi_gpu_numa_node_id`), GSP firmware mode/version, NvLink remote endpoint classification, and GPM SM occupancy / memory bandwidth utilization
    - Thermal thresholds (`all_smi_gpu_temperature_threshold_{slowdown,shutdown,max_operating,acoustic}_celsius`) and the canonical `all_smi_gpu_performance_state` gauge
    - Emitted only when the driver exposes the underlying NVML APIs; older drivers silently omit these metrics
    - Set `ALL_SMI_MOCK_HARDWARE_DETAILS=1` (with `--features mock` build) to have the mock emit the full extended hardware-detail set; when unset, the mock simulates an older driver
