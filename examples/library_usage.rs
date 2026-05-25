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

//! Example demonstrating the all-smi library API.
//!
//! This example shows how to use the high-level AllSmi API to query
//! GPU, CPU, memory, and process information.
//!
//! Run with: `cargo run --example library_usage`

use all_smi::prelude::*;

fn main() -> Result<()> {
    println!("=== all-smi Library Usage Example ===\n");

    // Initialize AllSmi with default configuration
    let smi = AllSmi::new()?;

    // ==========================================================================
    // GPU / NPU Information
    // ==========================================================================
    println!("--- GPU/NPU Information ---");
    let gpus = smi.get_gpu_info();

    if gpus.is_empty() {
        println!("No GPUs/NPUs detected on this system.");
    } else {
        println!("Found {} GPU(s)/NPU(s):\n", gpus.len());

        for (i, gpu) in gpus.iter().enumerate() {
            println!("  [{i}] {}", gpu.name);
            println!("      Type: {}", gpu.device_type);
            println!("      Utilization: {:.1}%", gpu.utilization);
            println!(
                "      Memory: {} MB / {} MB ({:.1}%)",
                gpu.used_memory / 1024 / 1024,
                gpu.total_memory / 1024 / 1024,
                if gpu.total_memory > 0 {
                    (gpu.used_memory as f64 / gpu.total_memory as f64) * 100.0
                } else {
                    0.0
                }
            );
            println!("      Temperature: {}C", gpu.temperature);
            println!("      Power: {:.1}W", gpu.power_consumption);
            println!("      Frequency: {} MHz", gpu.frequency);

            if let Some(cores) = gpu.gpu_core_count {
                println!("      GPU Cores: {cores}");
            }

            println!();
        }
    }

    // ==========================================================================
    // GPU/NPU Process Information
    // ==========================================================================
    println!("--- GPU/NPU Processes ---");
    let processes = smi.get_process_info();

    if processes.is_empty() {
        println!("No GPU/NPU processes running.");
    } else {
        println!("Found {} GPU process(es):\n", processes.len());

        for proc in processes.iter().take(5) {
            println!(
                "  PID {}: {} ({} MB GPU memory)",
                proc.pid,
                proc.process_name,
                proc.used_memory / 1024 / 1024
            );
        }

        if processes.len() > 5 {
            println!("  ... and {} more", processes.len() - 5);
        }
    }
    println!();

    // ==========================================================================
    // CPU Information
    // ==========================================================================
    println!("--- CPU Information ---");
    let cpus = smi.get_cpu_info();

    if cpus.is_empty() {
        println!("CPU information not available.");
    } else {
        for cpu in &cpus {
            println!("  Model: {}", cpu.cpu_model);
            println!("  Architecture: {}", cpu.architecture);
            println!(
                "  Cores: {} (Threads: {})",
                cpu.total_cores, cpu.total_threads
            );
            println!("  Sockets: {}", cpu.socket_count);
            println!("  Utilization: {:.1}%", cpu.utilization);
            println!(
                "  Frequency: {} MHz (Max: {} MHz)",
                cpu.base_frequency_mhz, cpu.max_frequency_mhz
            );

            if let Some(temp) = cpu.temperature {
                println!("  Temperature: {temp}C");
            }

            if let Some(power) = cpu.power_consumption {
                println!("  Power: {power:.1}W");
            }

            // Apple Silicon specific info
            if let Some(ref apple_info) = cpu.apple_silicon_info {
                println!("  Apple Silicon Details:");
                println!(
                    "    P-cores: {} ({:.1}% utilization)",
                    apple_info.p_core_count, apple_info.p_core_utilization
                );
                println!(
                    "    E-cores: {} ({:.1}% utilization)",
                    apple_info.e_core_count, apple_info.e_core_utilization
                );
                println!("    GPU cores: {}", apple_info.gpu_core_count);

                if let Some(p_freq) = apple_info.p_cluster_frequency_mhz {
                    println!("    P-cluster frequency: {p_freq} MHz");
                }
                if let Some(e_freq) = apple_info.e_cluster_frequency_mhz {
                    println!("    E-cluster frequency: {e_freq} MHz");
                }
            }
        }
    }
    println!();

    // ==========================================================================
    // Memory Information
    // ==========================================================================
    println!("--- Memory Information ---");
    let memory = smi.get_memory_info();

    if memory.is_empty() {
        println!("Memory information not available.");
    } else {
        for mem in &memory {
            let total_gb = mem.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let used_gb = mem.used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let available_gb = mem.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0;

            println!("  Total: {total_gb:.1} GB");
            println!("  Used: {used_gb:.1} GB ({:.1}%)", mem.utilization);
            println!("  Available: {available_gb:.1} GB");

            if mem.swap_total_bytes > 0 {
                let swap_total_gb = mem.swap_total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                let swap_used_gb = mem.swap_used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                println!("  Swap: {swap_used_gb:.1} GB / {swap_total_gb:.1} GB");
            }

            // Linux-specific metrics
            if mem.buffers_bytes > 0 || mem.cached_bytes > 0 {
                let buffers_mb = mem.buffers_bytes as f64 / 1024.0 / 1024.0;
                let cached_mb = mem.cached_bytes as f64 / 1024.0 / 1024.0;
                println!("  Buffers: {buffers_mb:.1} MB, Cached: {cached_mb:.1} MB");
            }
        }
    }
    println!();

    // ==========================================================================
    // Chassis Information
    // ==========================================================================
    println!("--- Chassis Information ---");
    if let Some(chassis) = smi.get_chassis_info() {
        if let Some(power) = chassis.total_power_watts {
            println!("  Total System Power: {power:.1}W");
        }

        if let Some(ref pressure) = chassis.thermal_pressure {
            println!("  Thermal Pressure: {pressure}");
        }

        if let Some(inlet) = chassis.inlet_temperature {
            println!("  Inlet Temperature: {inlet:.1}C");
        }

        if let Some(outlet) = chassis.outlet_temperature {
            println!("  Outlet Temperature: {outlet:.1}C");
        }

        if !chassis.fan_speeds.is_empty() {
            println!("  Fans:");
            for fan in &chassis.fan_speeds {
                println!(
                    "    {}: {} RPM / {} RPM",
                    fan.name, fan.speed_rpm, fan.max_rpm
                );
            }
        }

        if !chassis.psu_status.is_empty() {
            println!("  PSUs:");
            for psu in &chassis.psu_status {
                println!("    {}: {:?}", psu.name, psu.status);
            }
        }
    } else {
        println!("  Chassis information not available on this platform.");
    }
    println!();

    // ==========================================================================
    // Storage Information
    // ==========================================================================
    println!("--- Storage Information ---");
    let storage = smi.get_storage_info();

    if storage.is_empty() {
        println!("  No storage devices detected.");
    } else {
        println!("Found {} storage device(s):\n", storage.len());

        for s in &storage {
            let used_bytes = s.total_bytes - s.available_bytes;
            let usage_percent = if s.total_bytes > 0 {
                (used_bytes as f64 / s.total_bytes as f64) * 100.0
            } else {
                0.0
            };

            let total_gb = s.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let available_gb = s.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let used_gb = used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;

            println!("  [{}] {}", s.index, s.mount_point);
            println!(
                "      Total: {total_gb:.1} GB, Used: {used_gb:.1} GB, Available: {available_gb:.1} GB ({usage_percent:.1}% used)"
            );
        }
    }
    println!();

    // ==========================================================================
    // Targeted Refresh and Stable Correlation IDs
    // ==========================================================================
    // The `get_*_info()` getters return owned point-in-time snapshots, so the
    // copy you hold goes stale over time. Re-calling the full getter is one
    // option, but it re-enumerates every device. The helpers below let you
    // refresh a single entry of interest using a stable correlation key:
    //
    //   * GPU / NPU: `GpuInfo::uuid` (already unique per device)
    //   * CPU:       `CpuInfo::index` (0-based, stable, assigned by AllSmi)
    //   * Memory:    `MemoryInfo::index` (same scheme as CpuInfo)
    //
    // The pattern: snapshot once, hold onto the key, then refresh in place
    // whenever you need fresh numbers for that one device.
    println!("--- Targeted Refresh Demo ---");

    // GPU refresh by UUID — only runs if at least one GPU/NPU is present.
    if let Some(first_uuid) = gpus.first().map(|g| g.uuid.clone()) {
        // Fetch fresh data for just that one device.
        if let Some(fresh) = smi.get_gpu_by_uuid(&first_uuid) {
            println!(
                "  GPU {} refreshed by uuid: util={:.1}%, mem={} MB used",
                fresh.name,
                fresh.utilization,
                fresh.used_memory / 1024 / 1024
            );
        }

        // Or refresh an existing owned struct in place.
        let mut held = gpus[0].clone();
        if smi.refresh_gpu(&mut held) {
            println!(
                "  GPU {} refreshed in place: util now {:.1}%",
                held.name, held.utilization
            );
        } else {
            println!("  GPU {} disappeared between calls", held.name);
        }
    } else {
        println!("  No GPUs/NPUs detected — skipping targeted GPU refresh demo.");
    }

    // CPU refresh by index — CPU topology is static, so the index is stable
    // across calls.
    if let Some(mut held_cpu) = cpus.first().cloned() {
        let original_index = held_cpu.index;
        if smi.refresh_cpu(&mut held_cpu) {
            println!(
                "  CPU index {original_index} ({}) refreshed in place: util now {:.1}%",
                held_cpu.cpu_model, held_cpu.utilization
            );
        }
    }

    // Memory refresh by index.
    if let Some(mut held_mem) = memory.first().cloned() {
        let original_index = held_mem.index;
        if smi.refresh_memory(&mut held_mem) {
            let total_gb = held_mem.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            println!(
                "  Memory index {original_index} refreshed in place: util now {:.1}% (total {total_gb:.1} GB)",
                held_mem.utilization
            );
        }
    }
    println!();

    // ==========================================================================
    // Summary
    // ==========================================================================
    println!("--- Summary ---");
    println!("  GPU readers: {}", smi.gpu_reader_count());
    println!("  Has GPUs: {}", smi.has_gpus());
    println!("  Has CPU monitoring: {}", smi.has_cpu_monitoring());
    println!("  Has memory monitoring: {}", smi.has_memory_monitoring());
    println!("  Has storage monitoring: {}", smi.has_storage_monitoring());

    Ok(())
}
