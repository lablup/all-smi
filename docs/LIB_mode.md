# all-smi Library API Manual

This document provides comprehensive documentation for using `all-smi` as a Rust library in your projects.

## Table of Contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Core API](#core-api)
  - [AllSmi Client](#allsmi-client)
  - [Configuration](#configuration)
  - [Error Handling](#error-handling)
- [Data Types](#data-types)
  - [GpuInfo](#gpuinfo)
  - [ProcessInfo](#processinfo)
  - [CpuInfo](#cpuinfo)
  - [MemoryInfo](#memoryinfo)
  - [StorageInfo](#storageinfo)
  - [ChassisInfo](#chassisinfo)
- [Platform Support](#platform-support)
- [Advanced Usage](#advanced-usage)
  - [Using the Prelude](#using-the-prelude)
  - [Custom Configuration](#custom-configuration)
  - [Thread Safety](#thread-safety)
  - [Low-Level Access](#low-level-access)
- [Complete Examples](#complete-examples)
- [Best Practices](#best-practices)

---

## Overview

The `all-smi` library provides a unified, cross-platform API for monitoring hardware accelerators (GPUs, NPUs, TPUs), CPUs, and system memory. It abstracts away platform-specific details, allowing you to write hardware monitoring code that works across:

- **NVIDIA GPUs** (via NVML)
- **AMD GPUs** (via ROCm SMI)
- **Apple Silicon** (via IOReport/SMC)
- **Intel Gaudi NPUs** (via hl-smi)
- **Furiosa NPUs** (via furiosa-smi)
- **Rebellions NPUs** (via rbln-stat)
- **Tenstorrent NPUs** (via tt-smi)
- **Google TPUs** (via libtpu)

## Installation

Add `all-smi` to your `Cargo.toml`:

```toml
[dependencies]
all_smi = "0.15"
```

Or using cargo:

```bash
cargo add all_smi
```

## Quick Start

The simplest way to get started is with the `AllSmi` client:

```rust
use all_smi::{AllSmi, Result};

fn main() -> Result<()> {
    // Initialize the client with auto-detection
    let smi = AllSmi::new()?;

    // Query GPU information
    for gpu in smi.get_gpu_info() {
        println!("{}: {}% utilization, {:.1}W power",
            gpu.name, gpu.utilization, gpu.power_consumption);
    }

    // Query CPU information
    for cpu in smi.get_cpu_info() {
        println!("{}: {:.1}% utilization", cpu.cpu_model, cpu.utilization);
    }

    // Query memory information
    for mem in smi.get_memory_info() {
        let used_gb = mem.used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let total_gb = mem.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        println!("Memory: {:.1} GB / {:.1} GB ({:.1}%)",
            used_gb, total_gb, mem.utilization);
    }

    // Query storage information
    for storage in smi.get_storage_info() {
        let total_gb = storage.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let available_gb = storage.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let used_gb = total_gb - available_gb;
        let util = if storage.total_bytes > 0 {
            ((storage.total_bytes - storage.available_bytes) as f64 / storage.total_bytes as f64) * 100.0
        } else {
            0.0
        };
        println!("{}: {:.1} GB / {:.1} GB ({:.1}%)",
            storage.mount_point, used_gb, total_gb, util);
    }

    Ok(())
}
```

---

## Core API

### AllSmi Client

The `AllSmi` struct is the main entry point for the library. It provides a high-level, ergonomic API for querying hardware information.

#### Creating an Instance

```rust
use all_smi::{AllSmi, Result};

fn main() -> Result<()> {
    // Default initialization
    let smi = AllSmi::new()?;

    // With custom configuration
    let smi = AllSmi::with_config(
        AllSmiConfig::new()
            .sample_interval(500)  // 500ms sampling
            .verbose(true)         // Enable verbose logging
    )?;

    Ok(())
}
```

#### Available Methods

| Method | Return Type | Description |
|--------|-------------|-------------|
| `new()` | `Result<AllSmi>` | Create instance with default config |
| `with_config(config)` | `Result<AllSmi>` | Create instance with custom config |
| `get_gpu_info()` | `Vec<GpuInfo>` | Get all GPU/NPU information |
| `get_process_info()` | `Vec<ProcessInfo>` | Get GPU process information |
| `get_cpu_info()` | `Vec<CpuInfo>` | Get CPU information |
| `get_memory_info()` | `Vec<MemoryInfo>` | Get system memory information |
| `get_storage_info()` | `Vec<StorageInfo>` | Get disk/storage information |
| `get_chassis_info()` | `Option<ChassisInfo>` | Get chassis-level information |
| `has_gpus()` | `bool` | Check if any GPUs are detected |
| `has_cpu_monitoring()` | `bool` | Check if CPU monitoring is available |
| `has_memory_monitoring()` | `bool` | Check if memory monitoring is available |
| `has_storage_monitoring()` | `bool` | Check if storage monitoring is available |
| `gpu_reader_count()` | `usize` | Get number of GPU reader types |

### Configuration

Use `AllSmiConfig` to customize the client behavior:

```rust
use all_smi::{AllSmi, AllSmiConfig, Result};

fn main() -> Result<()> {
    let config = AllSmiConfig::new()
        .sample_interval(1000)  // Sample interval in milliseconds
        .verbose(true);         // Print warnings during init

    let smi = AllSmi::with_config(config)?;
    Ok(())
}
```

#### Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `sample_interval_ms` | `u64` | `1000` | Sampling interval for platform managers |
| `verbose` | `bool` | `false` | Enable verbose warning output |

### Error Handling

The library uses a unified `Error` type with the following variants:

```rust
use all_smi::{AllSmi, Error, Result};

fn main() {
    match AllSmi::new() {
        Ok(smi) => {
            println!("Initialized successfully");
        }
        Err(Error::PlatformInit(msg)) => {
            eprintln!("Platform init failed: {}", msg);
        }
        Err(Error::NoDevicesFound) => {
            eprintln!("No devices found");
        }
        Err(Error::DeviceAccess(msg)) => {
            eprintln!("Device access error: {}", msg);
        }
        Err(Error::PermissionDenied(msg)) => {
            eprintln!("Permission denied: {}", msg);
        }
        Err(Error::NotSupported(msg)) => {
            eprintln!("Not supported: {}", msg);
        }
        Err(Error::Io(e)) => {
            eprintln!("I/O error: {}", e);
        }
    }
}
```

#### Error Variants

| Variant | Description |
|---------|-------------|
| `PlatformInit(String)` | Platform initialization failed (e.g., NVML not found) |
| `NoDevicesFound` | No supported devices detected |
| `DeviceAccess(String)` | Cannot access or query a detected device |
| `PermissionDenied(String)` | Insufficient permissions (e.g., AMD GPU without sudo) |
| `NotSupported(String)` | Feature not available on this platform |
| `Io(std::io::Error)` | Standard I/O error |

---

## Data Types

### GpuInfo

Contains information about a GPU or NPU device.

```rust
use all_smi::{AllSmi, Result};

fn print_gpu_details() -> Result<()> {
    let smi = AllSmi::new()?;

    for gpu in smi.get_gpu_info() {
        // Basic identification
        println!("Name: {}", gpu.name);
        println!("UUID: {}", gpu.uuid);
        println!("Type: {}", gpu.device_type);  // "GPU", "NPU", etc.

        // Utilization metrics
        println!("Utilization: {:.1}%", gpu.utilization);
        println!("ANE Utilization: {:.1}%", gpu.ane_utilization);

        // Optional utilizations
        if let Some(dla) = gpu.dla_utilization {
            println!("DLA Utilization: {:.1}%", dla);
        }
        if let Some(tc) = gpu.tensorcore_utilization {
            println!("TensorCore Utilization: {:.1}%", tc);
        }

        // Memory
        let used_mb = gpu.used_memory / 1024 / 1024;
        let total_mb = gpu.total_memory / 1024 / 1024;
        let mem_pct = if gpu.total_memory > 0 {
            (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0
        } else {
            0.0
        };
        println!("Memory: {} MB / {} MB ({:.1}%)", used_mb, total_mb, mem_pct);

        // Thermal & Power
        println!("Temperature: {}°C", gpu.temperature);
        println!("Power: {:.1}W", gpu.power_consumption);
        println!("Frequency: {} MHz", gpu.frequency);

        // Optional core count
        if let Some(cores) = gpu.gpu_core_count {
            println!("GPU Cores: {}", cores);
        }

        // Access extended details
        for (key, value) in &gpu.detail {
            println!("  {}: {}", key, value);
        }
    }

    Ok(())
}
```

#### GpuInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `uuid` | `String` | Unique device identifier |
| `name` | `String` | Device name (e.g., "NVIDIA GeForce RTX 4090") |
| `device_type` | `String` | Device category ("GPU", "NPU", etc.) |
| `utilization` | `f64` | GPU utilization percentage (0-100) |
| `ane_utilization` | `f64` | ANE utilization (Apple Silicon) |
| `dla_utilization` | `Option<f64>` | DLA utilization (NVIDIA Jetson) |
| `tensorcore_utilization` | `Option<f64>` | TensorCore utilization (TPU) |
| `temperature` | `u32` | Temperature in Celsius |
| `used_memory` | `u64` | Used memory in bytes |
| `total_memory` | `u64` | Total memory in bytes |
| `frequency` | `u32` | Current frequency in MHz |
| `power_consumption` | `f64` | Power consumption in Watts |
| `gpu_core_count` | `Option<u32>` | Number of GPU cores |
| `detail` | `HashMap<String, String>` | Platform-specific details |

### ProcessInfo

Contains information about processes using GPU resources.

```rust
use all_smi::{AllSmi, Result};

fn list_gpu_processes() -> Result<()> {
    let smi = AllSmi::new()?;

    for proc in smi.get_process_info() {
        // Basic process info
        println!("PID: {}", proc.pid);
        println!("Name: {}", proc.process_name);
        println!("Command: {}", proc.command);
        println!("User: {}", proc.user);

        // GPU resources
        println!("GPU Memory: {} MB", proc.used_memory / 1024 / 1024);
        println!("GPU Utilization: {:.1}%", proc.gpu_utilization);
        println!("Device UUID: {}", proc.device_uuid);

        // System resources
        println!("CPU: {:.1}%", proc.cpu_percent);
        println!("System Memory: {:.1}%", proc.memory_percent);
        println!("RSS: {} MB", proc.memory_rss / 1024 / 1024);
        println!("VMS: {} MB", proc.memory_vms / 1024 / 1024);

        // Process details
        println!("State: {}", proc.state);
        println!("Threads: {}", proc.threads);
        println!("Priority: {}", proc.priority);
        println!("Nice: {}", proc.nice_value);
        println!("CPU Time: {}s", proc.cpu_time);
        println!("Started: {}", proc.start_time);
        println!("Parent PID: {}", proc.ppid);
    }

    Ok(())
}
```

#### ProcessInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `device_id` | `usize` | GPU index (internal) |
| `device_uuid` | `String` | GPU UUID |
| `pid` | `u32` | Process ID |
| `process_name` | `String` | Process name |
| `used_memory` | `u64` | GPU memory usage in bytes |
| `cpu_percent` | `f64` | CPU usage percentage |
| `memory_percent` | `f64` | System memory usage percentage |
| `memory_rss` | `u64` | Resident Set Size in bytes |
| `memory_vms` | `u64` | Virtual Memory Size in bytes |
| `user` | `String` | User name |
| `state` | `String` | Process state (R, S, D, etc.) |
| `start_time` | `String` | Process start time |
| `cpu_time` | `u64` | Total CPU time in seconds |
| `command` | `String` | Full command line |
| `ppid` | `u32` | Parent process ID |
| `threads` | `u32` | Number of threads |
| `uses_gpu` | `bool` | Whether process uses GPU |
| `priority` | `i32` | Process priority |
| `nice_value` | `i32` | Nice value |
| `gpu_utilization` | `f64` | GPU utilization percentage |

### CpuInfo

Contains CPU information including architecture-specific details.

```rust
use all_smi::{AllSmi, Result, CpuPlatformType, CoreType};

fn print_cpu_info() -> Result<()> {
    let smi = AllSmi::new()?;

    for cpu in smi.get_cpu_info() {
        // Basic info
        println!("Model: {}", cpu.cpu_model);
        println!("Architecture: {}", cpu.architecture);

        // Platform type
        match cpu.platform_type {
            CpuPlatformType::Intel => println!("Platform: Intel"),
            CpuPlatformType::Amd => println!("Platform: AMD"),
            CpuPlatformType::AppleSilicon => println!("Platform: Apple Silicon"),
            CpuPlatformType::Arm => println!("Platform: ARM"),
            CpuPlatformType::Other => println!("Platform: Other"),
        }

        // Topology
        println!("Sockets: {}", cpu.socket_count);
        println!("Cores: {}", cpu.total_cores);
        println!("Threads: {}", cpu.total_threads);
        println!("Cache: {} MB", cpu.cache_size_mb);

        // Performance
        println!("Utilization: {:.1}%", cpu.utilization);
        println!("Base Frequency: {} MHz", cpu.base_frequency_mhz);
        println!("Max Frequency: {} MHz", cpu.max_frequency_mhz);

        // Optional metrics
        if let Some(temp) = cpu.temperature {
            println!("Temperature: {}°C", temp);
        }
        if let Some(power) = cpu.power_consumption {
            println!("Power: {:.1}W", power);
        }

        // Per-core utilization
        for core in &cpu.per_core_utilization {
            let core_type_str = match core.core_type {
                CoreType::Performance => "P",
                CoreType::Efficiency => "E",
                CoreType::Standard => "S",
            };
            println!("  Core {} ({}): {:.1}%",
                core.core_id, core_type_str, core.utilization);
        }

        // Apple Silicon specific
        if let Some(ref apple) = cpu.apple_silicon_info {
            println!("\nApple Silicon Details:");
            println!("  P-cores: {} ({:.1}% util)",
                apple.p_core_count, apple.p_core_utilization);
            println!("  E-cores: {} ({:.1}% util)",
                apple.e_core_count, apple.e_core_utilization);
            println!("  GPU Cores: {}", apple.gpu_core_count);

            if let Some(freq) = apple.p_cluster_frequency_mhz {
                println!("  P-cluster Frequency: {} MHz", freq);
            }
            if let Some(freq) = apple.e_cluster_frequency_mhz {
                println!("  E-cluster Frequency: {} MHz", freq);
            }
        }
    }

    Ok(())
}
```

#### CpuInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `cpu_model` | `String` | CPU model name |
| `architecture` | `String` | Architecture ("x86_64", "arm64") |
| `platform_type` | `CpuPlatformType` | Platform type enum |
| `socket_count` | `u32` | Number of CPU sockets |
| `total_cores` | `u32` | Total logical cores |
| `total_threads` | `u32` | Total threads |
| `base_frequency_mhz` | `u32` | Base CPU frequency |
| `max_frequency_mhz` | `u32` | Maximum CPU frequency |
| `cache_size_mb` | `u32` | Total cache size in MB |
| `utilization` | `f64` | Overall CPU utilization |
| `temperature` | `Option<u32>` | CPU temperature (if available) |
| `power_consumption` | `Option<f64>` | Power consumption in Watts |
| `per_socket_info` | `Vec<CpuSocketInfo>` | Per-socket information |
| `apple_silicon_info` | `Option<AppleSiliconCpuInfo>` | Apple Silicon details |
| `per_core_utilization` | `Vec<CoreUtilization>` | Per-core utilization |

### MemoryInfo

Contains system memory information.

```rust
use all_smi::{AllSmi, Result};

fn print_memory_info() -> Result<()> {
    let smi = AllSmi::new()?;

    for mem in smi.get_memory_info() {
        // Convert to human-readable units
        let to_gb = |bytes: u64| bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let to_mb = |bytes: u64| bytes as f64 / 1024.0 / 1024.0;

        // Main memory
        println!("Total: {:.1} GB", to_gb(mem.total_bytes));
        println!("Used: {:.1} GB ({:.1}%)", to_gb(mem.used_bytes), mem.utilization);
        println!("Available: {:.1} GB", to_gb(mem.available_bytes));
        println!("Free: {:.1} GB", to_gb(mem.free_bytes));

        // Linux-specific (buffers/cache)
        if mem.buffers_bytes > 0 || mem.cached_bytes > 0 {
            println!("Buffers: {:.1} MB", to_mb(mem.buffers_bytes));
            println!("Cached: {:.1} MB", to_mb(mem.cached_bytes));
        }

        // Swap
        if mem.swap_total_bytes > 0 {
            println!("Swap Total: {:.1} GB", to_gb(mem.swap_total_bytes));
            println!("Swap Used: {:.1} GB", to_gb(mem.swap_used_bytes));
            println!("Swap Free: {:.1} GB", to_gb(mem.swap_free_bytes));
        }
    }

    Ok(())
}
```

#### MemoryInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `total_bytes` | `u64` | Total system memory |
| `used_bytes` | `u64` | Used memory |
| `available_bytes` | `u64` | Available memory |
| `free_bytes` | `u64` | Free memory |
| `buffers_bytes` | `u64` | Buffer memory (Linux) |
| `cached_bytes` | `u64` | Cached memory (Linux) |
| `swap_total_bytes` | `u64` | Total swap space |
| `swap_used_bytes` | `u64` | Used swap space |
| `swap_free_bytes` | `u64` | Free swap space |
| `utilization` | `f64` | Memory utilization percentage |

### StorageInfo

Contains disk/storage information for mounted filesystems.

```rust
use all_smi::{AllSmi, Result};

fn print_storage_info() -> Result<()> {
    let smi = AllSmi::new()?;

    for storage in smi.get_storage_info() {
        // Convert to human-readable units
        let to_gb = |bytes: u64| bytes as f64 / 1024.0 / 1024.0 / 1024.0;

        // Mount point identification
        println!("Mount Point: {}", storage.mount_point);
        println!("Index: {}", storage.index);

        // Space metrics
        let total_gb = to_gb(storage.total_bytes);
        let available_gb = to_gb(storage.available_bytes);
        let used_gb = total_gb - available_gb;
        let utilization = if storage.total_bytes > 0 {
            ((storage.total_bytes - storage.available_bytes) as f64
                / storage.total_bytes as f64) * 100.0
        } else {
            0.0
        };

        println!("Total: {:.1} GB", total_gb);
        println!("Used: {:.1} GB ({:.1}%)", used_gb, utilization);
        println!("Available: {:.1} GB", available_gb);

        // Host identification (for remote monitoring)
        println!("Host ID: {}", storage.host_id);
        println!("Hostname: {}", storage.hostname);
    }

    Ok(())
}
```

#### StorageInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `mount_point` | `String` | The filesystem mount point (e.g., "/", "/home") |
| `total_bytes` | `u64` | Total disk space in bytes |
| `available_bytes` | `u64` | Available disk space in bytes |
| `host_id` | `String` | Host identifier for remote monitoring |
| `hostname` | `String` | DNS hostname of the server |
| `index` | `u32` | Index for ordering multiple disks |

### ChassisInfo

Contains system-wide chassis and node-level information.

```rust
use all_smi::{AllSmi, Result};

fn print_chassis_info() -> Result<()> {
    let smi = AllSmi::new()?;

    if let Some(chassis) = smi.get_chassis_info() {
        // Power
        if let Some(power) = chassis.total_power_watts {
            println!("Total System Power: {:.1}W", power);
        }

        // Thermal
        if let Some(temp) = chassis.inlet_temperature {
            println!("Inlet Temperature: {:.1}°C", temp);
        }
        if let Some(temp) = chassis.outlet_temperature {
            println!("Outlet Temperature: {:.1}°C", temp);
        }
        if let Some(ref pressure) = chassis.thermal_pressure {
            println!("Thermal Pressure: {}", pressure);
        }

        // Fan speeds
        if !chassis.fan_speeds.is_empty() {
            println!("\nFans:");
            for fan in &chassis.fan_speeds {
                let pct = if fan.max_rpm > 0 {
                    (fan.speed_rpm as f64 / fan.max_rpm as f64) * 100.0
                } else {
                    0.0
                };
                println!("  {} (ID {}): {} RPM / {} RPM ({:.0}%)",
                    fan.name, fan.id, fan.speed_rpm, fan.max_rpm, pct);
            }
        }

        // PSU status
        if !chassis.psu_status.is_empty() {
            println!("\nPSUs:");
            for psu in &chassis.psu_status {
                let status_str = match psu.status {
                    all_smi::prelude::PsuStatus::Ok => "OK",
                    all_smi::prelude::PsuStatus::Warning => "Warning",
                    all_smi::prelude::PsuStatus::Critical => "Critical",
                    all_smi::prelude::PsuStatus::Unknown => "Unknown",
                };
                print!("  {} (ID {}): {}", psu.name, psu.id, status_str);
                if let Some(power) = psu.power_watts {
                    print!(" ({:.1}W)", power);
                }
                println!();
            }
        }

        // Platform-specific details
        for (key, value) in &chassis.detail {
            println!("  {}: {}", key, value);
        }
    } else {
        println!("Chassis information not available");
    }

    Ok(())
}
```

#### ChassisInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `total_power_watts` | `Option<f64>` | Combined power (CPU+GPU+ANE) |
| `inlet_temperature` | `Option<f64>` | Inlet temperature |
| `outlet_temperature` | `Option<f64>` | Outlet temperature |
| `thermal_pressure` | `Option<String>` | Thermal pressure level |
| `fan_speeds` | `Vec<FanInfo>` | Fan speed information |
| `psu_status` | `Vec<PsuInfo>` | PSU status information |
| `detail` | `HashMap<String, String>` | Platform-specific details |

---

## Platform Support

### Device Support Matrix

| Platform | Device Types | Requirements |
|----------|--------------|--------------|
| Linux | NVIDIA GPU, AMD GPU, Intel Gaudi, Furiosa, Rebellions, Tenstorrent, TPU | Vendor SDKs |
| macOS | Apple Silicon GPU/ANE | macOS 12+ |
| Windows | NVIDIA GPU | NVML |

### Platform-Specific Features

#### Apple Silicon (macOS)
- GPU/ANE utilization via IOReport
- Per-core utilization (P-cores/E-cores)
- Thermal pressure monitoring
- Unified memory metrics

#### NVIDIA (Linux/Windows)
- Full NVML support
- CUDA compute capability
- NVLink topology
- ECC error counts

#### Intel Gaudi (Linux)
- hl-smi integration
- HBM memory monitoring
- Multi-chip module support

---

## Advanced Usage

### Using the Prelude

For convenience, import everything with the prelude:

```rust
use all_smi::prelude::*;

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    // All types are available: GpuInfo, CpuInfo, MemoryInfo, StorageInfo, etc.
    let gpus: Vec<GpuInfo> = smi.get_gpu_info();
    let cpus: Vec<CpuInfo> = smi.get_cpu_info();
    let memory: Vec<MemoryInfo> = smi.get_memory_info();
    let storage: Vec<StorageInfo> = smi.get_storage_info();
    let chassis: Option<ChassisInfo> = smi.get_chassis_info();

    Ok(())
}
```

### Custom Configuration

Fine-tune the client behavior:

```rust
use all_smi::{AllSmi, AllSmiConfig, Result};

fn main() -> Result<()> {
    // Fast sampling for real-time monitoring
    let fast_config = AllSmiConfig::new()
        .sample_interval(100)  // 100ms
        .verbose(false);

    // Slower sampling for background monitoring
    let slow_config = AllSmiConfig::new()
        .sample_interval(5000)  // 5 seconds
        .verbose(false);

    let smi = AllSmi::with_config(fast_config)?;
    Ok(())
}
```

### Thread Safety

`AllSmi` is `Send + Sync` and can be safely shared across threads:

```rust
use all_smi::{AllSmi, Result};
use std::sync::Arc;
use std::thread;

fn main() -> Result<()> {
    let smi = Arc::new(AllSmi::new()?);

    let handles: Vec<_> = (0..4).map(|i| {
        let smi = Arc::clone(&smi);
        thread::spawn(move || {
            let gpus = smi.get_gpu_info();
            println!("Thread {}: Found {} GPUs", i, gpus.len());
        })
    }).collect();

    for handle in handles {
        handle.join().unwrap();
    }

    Ok(())
}
```

#### With Tokio (Async)

```rust
use all_smi::{AllSmi, Result};
use std::sync::Arc;
use tokio::task;

#[tokio::main]
async fn main() -> Result<()> {
    let smi = Arc::new(AllSmi::new()?);

    let smi_clone = Arc::clone(&smi);
    let gpu_task = task::spawn_blocking(move || {
        smi_clone.get_gpu_info()
    });

    let smi_clone = Arc::clone(&smi);
    let cpu_task = task::spawn_blocking(move || {
        smi_clone.get_cpu_info()
    });

    let (gpus, cpus) = tokio::try_join!(gpu_task, cpu_task)?;

    println!("GPUs: {}, CPUs: {}", gpus.len(), cpus.len());
    Ok(())
}
```

### Low-Level Access

For advanced use cases, you can access the underlying reader traits:

```rust
use all_smi::prelude::*;

fn main() -> Result<()> {
    // Get raw readers for custom handling
    let gpu_readers: Vec<Box<dyn GpuReader>> = get_gpu_readers();
    let cpu_readers: Vec<Box<dyn CpuReader>> = get_cpu_readers();
    let memory_readers: Vec<Box<dyn MemoryReader>> = get_memory_readers();
    let storage_reader: Box<dyn StorageReader> = create_storage_reader();
    let chassis_reader: Box<dyn ChassisReader> = create_chassis_reader();

    // Use readers directly
    for reader in &gpu_readers {
        for gpu in reader.get_gpu_info() {
            println!("{}: {}%", gpu.name, gpu.utilization);
        }
    }

    // Use storage reader directly
    for storage in storage_reader.get_storage_info() {
        println!("{}: {} bytes available", storage.mount_point, storage.available_bytes);
    }

    Ok(())
}
```

---

## Complete Examples

### Monitoring Dashboard

A complete example for building a monitoring dashboard:

```rust
use all_smi::prelude::*;
use std::time::{Duration, Instant};
use std::thread;

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    println!("Hardware Monitoring Dashboard");
    println!("============================\n");

    loop {
        let start = Instant::now();

        // Clear screen (Unix)
        print!("\x1B[2J\x1B[1;1H");

        println!("=== GPU/NPU Status ===");
        for gpu in smi.get_gpu_info() {
            let mem_pct = if gpu.total_memory > 0 {
                (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0
            } else {
                0.0
            };

            println!("[{}] {} | Util: {:5.1}% | Mem: {:5.1}% | Temp: {:3}°C | Power: {:6.1}W",
                gpu.device_type, gpu.name,
                gpu.utilization, mem_pct,
                gpu.temperature, gpu.power_consumption);
        }

        println!("\n=== CPU Status ===");
        for cpu in smi.get_cpu_info() {
            print!("{} | Util: {:5.1}%", cpu.cpu_model, cpu.utilization);
            if let Some(temp) = cpu.temperature {
                print!(" | Temp: {}°C", temp);
            }
            if let Some(power) = cpu.power_consumption {
                print!(" | Power: {:.1}W", power);
            }
            println!();
        }

        println!("\n=== Memory Status ===");
        for mem in smi.get_memory_info() {
            let used_gb = mem.used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let total_gb = mem.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            println!("RAM: {:6.2} GB / {:6.2} GB ({:5.1}%)",
                used_gb, total_gb, mem.utilization);
        }

        println!("\n=== Storage Status ===");
        for storage in smi.get_storage_info() {
            let total_gb = storage.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let available_gb = storage.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let used_gb = total_gb - available_gb;
            let util = if storage.total_bytes > 0 {
                ((storage.total_bytes - storage.available_bytes) as f64
                    / storage.total_bytes as f64) * 100.0
            } else {
                0.0
            };
            println!("{}: {:6.2} GB / {:6.2} GB ({:5.1}%)",
                storage.mount_point, used_gb, total_gb, util);
        }

        println!("\n=== System Power ===");
        if let Some(chassis) = smi.get_chassis_info() {
            if let Some(power) = chassis.total_power_watts {
                println!("Total System Power: {:.1}W", power);
            }
        }

        let elapsed = start.elapsed();
        println!("\n[Updated in {:?}]", elapsed);

        thread::sleep(Duration::from_secs(1));
    }
}
```

### GPU Process Monitor

Monitor processes using GPU resources:

```rust
use all_smi::prelude::*;

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    println!("{:>8} {:>8} {:>10} {:>6} {:>8}  {}",
        "PID", "GPU_MEM", "GPU_UTIL", "CPU%", "MEM%", "COMMAND");
    println!("{}", "-".repeat(70));

    let mut processes = smi.get_process_info();

    // Sort by GPU memory usage (descending)
    processes.sort_by(|a, b| b.used_memory.cmp(&a.used_memory));

    for proc in processes.iter().take(20) {
        let mem_mb = proc.used_memory / 1024 / 1024;
        let cmd = if proc.command.len() > 30 {
            format!("{}...", &proc.command[..27])
        } else {
            proc.command.clone()
        };

        println!("{:>8} {:>7}M {:>9.1}% {:>5.1}% {:>7.1}%  {}",
            proc.pid, mem_mb, proc.gpu_utilization,
            proc.cpu_percent, proc.memory_percent, cmd);
    }

    Ok(())
}
```

### JSON Export

Export hardware information as JSON:

```rust
use all_smi::prelude::*;
use serde_json::json;

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    let gpus = smi.get_gpu_info();
    let cpus = smi.get_cpu_info();
    let memory = smi.get_memory_info();
    let storage = smi.get_storage_info();
    let chassis = smi.get_chassis_info();

    let report = json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "gpus": gpus,
        "cpus": cpus,
        "memory": memory,
        "storage": storage,
        "chassis": chassis,
    });

    println!("{}", serde_json::to_string_pretty(&report)?);

    Ok(())
}
```

### Resource Threshold Alerting

Implement threshold-based alerting:

```rust
use all_smi::prelude::*;

struct Thresholds {
    gpu_util: f64,
    gpu_temp: u32,
    gpu_mem_pct: f64,
    cpu_util: f64,
    mem_util: f64,
    storage_util: f64,
}

fn check_thresholds(smi: &AllSmi, thresholds: &Thresholds) -> Vec<String> {
    let mut alerts = Vec::new();

    // Check GPU thresholds
    for gpu in smi.get_gpu_info() {
        if gpu.utilization > thresholds.gpu_util {
            alerts.push(format!("ALERT: {} utilization {:.1}% > {:.1}%",
                gpu.name, gpu.utilization, thresholds.gpu_util));
        }

        if gpu.temperature > thresholds.gpu_temp {
            alerts.push(format!("ALERT: {} temperature {}°C > {}°C",
                gpu.name, gpu.temperature, thresholds.gpu_temp));
        }

        if gpu.total_memory > 0 {
            let mem_pct = (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0;
            if mem_pct > thresholds.gpu_mem_pct {
                alerts.push(format!("ALERT: {} memory {:.1}% > {:.1}%",
                    gpu.name, mem_pct, thresholds.gpu_mem_pct));
            }
        }
    }

    // Check CPU thresholds
    for cpu in smi.get_cpu_info() {
        if cpu.utilization > thresholds.cpu_util {
            alerts.push(format!("ALERT: CPU utilization {:.1}% > {:.1}%",
                cpu.utilization, thresholds.cpu_util));
        }
    }

    // Check memory thresholds
    for mem in smi.get_memory_info() {
        if mem.utilization > thresholds.mem_util {
            alerts.push(format!("ALERT: Memory utilization {:.1}% > {:.1}%",
                mem.utilization, thresholds.mem_util));
        }
    }

    // Check storage thresholds
    for storage in smi.get_storage_info() {
        if storage.total_bytes > 0 {
            let util = ((storage.total_bytes - storage.available_bytes) as f64
                / storage.total_bytes as f64) * 100.0;
            if util > thresholds.storage_util {
                alerts.push(format!("ALERT: Storage {} utilization {:.1}% > {:.1}%",
                    storage.mount_point, util, thresholds.storage_util));
            }
        }
    }

    alerts
}

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    let thresholds = Thresholds {
        gpu_util: 90.0,
        gpu_temp: 80,
        gpu_mem_pct: 90.0,
        cpu_util: 95.0,
        mem_util: 90.0,
        storage_util: 85.0,
    };

    let alerts = check_thresholds(&smi, &thresholds);

    if alerts.is_empty() {
        println!("All systems nominal");
    } else {
        for alert in alerts {
            eprintln!("{}", alert);
        }
    }

    Ok(())
}
```

---

## Best Practices

### 1. Handle Missing Hardware Gracefully

```rust
use all_smi::{AllSmi, Result};

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    // Don't assume hardware exists
    let gpus = smi.get_gpu_info();
    if gpus.is_empty() {
        println!("No GPUs detected - running in CPU-only mode");
    } else {
        for gpu in gpus {
            // Process GPU info
        }
    }

    Ok(())
}
```

### 2. Use Appropriate Sampling Intervals

```rust
use all_smi::{AllSmi, AllSmiConfig, Result};

fn main() -> Result<()> {
    // For real-time dashboards: 100-500ms
    let realtime_config = AllSmiConfig::new().sample_interval(200);

    // For background monitoring: 1000-5000ms
    let background_config = AllSmiConfig::new().sample_interval(2000);

    // For one-shot queries: use default (1000ms)
    let oneshot = AllSmi::new()?;

    Ok(())
}
```

### 3. Reuse AllSmi Instances

```rust
use all_smi::{AllSmi, Result};
use std::sync::Arc;

// GOOD: Reuse single instance
fn main() -> Result<()> {
    let smi = Arc::new(AllSmi::new()?);

    // Share across your application
    let gpu_monitor = smi.clone();
    let cpu_monitor = smi.clone();

    Ok(())
}

// BAD: Creating new instances frequently
fn bad_example() -> Result<()> {
    loop {
        let smi = AllSmi::new()?;  // Don't do this!
        let _ = smi.get_gpu_info();
    }
}
```

### 4. Handle Platform Differences

```rust
use all_smi::{AllSmi, Result};

fn main() -> Result<()> {
    let smi = AllSmi::new()?;

    for cpu in smi.get_cpu_info() {
        // Apple Silicon has special info
        if let Some(ref apple) = cpu.apple_silicon_info {
            println!("P-cores: {}, E-cores: {}",
                apple.p_core_count, apple.e_core_count);
        }

        // Generic info works everywhere
        println!("Model: {}", cpu.cpu_model);
        println!("Utilization: {:.1}%", cpu.utilization);
    }

    Ok(())
}
```

### 5. Log Initialization Errors

```rust
use all_smi::{AllSmi, AllSmiConfig, Result};

fn main() -> Result<()> {
    // Enable verbose mode in development/debugging
    let config = AllSmiConfig::new()
        .verbose(cfg!(debug_assertions));

    let smi = AllSmi::with_config(config)?;

    Ok(())
}
```

---

## See Also

- [CLI Documentation](../README.md) - Command-line interface usage
- [Architecture Overview](ARCHITECTURE.md) - Internal architecture details
- [API Example](../examples/library_usage.rs) - Full working example

---

## License

Copyright 2025 Lablup Inc. and Jeongkyu Shin

Licensed under the Apache License, Version 2.0.
