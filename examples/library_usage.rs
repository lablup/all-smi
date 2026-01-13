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
            println!("  [{}] {}", i, gpu.name);
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
                println!("      GPU Cores: {}", cores);
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
                println!("  Temperature: {}C", temp);
            }

            if let Some(power) = cpu.power_consumption {
                println!("  Power: {:.1}W", power);
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
                    println!("    P-cluster frequency: {} MHz", p_freq);
                }
                if let Some(e_freq) = apple_info.e_cluster_frequency_mhz {
                    println!("    E-cluster frequency: {} MHz", e_freq);
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

            println!("  Total: {:.1} GB", total_gb);
            println!("  Used: {:.1} GB ({:.1}%)", used_gb, mem.utilization);
            println!("  Available: {:.1} GB", available_gb);

            if mem.swap_total_bytes > 0 {
                let swap_total_gb = mem.swap_total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                let swap_used_gb = mem.swap_used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                println!("  Swap: {:.1} GB / {:.1} GB", swap_used_gb, swap_total_gb);
            }

            // Linux-specific metrics
            if mem.buffers_bytes > 0 || mem.cached_bytes > 0 {
                let buffers_mb = mem.buffers_bytes as f64 / 1024.0 / 1024.0;
                let cached_mb = mem.cached_bytes as f64 / 1024.0 / 1024.0;
                println!(
                    "  Buffers: {:.1} MB, Cached: {:.1} MB",
                    buffers_mb, cached_mb
                );
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
            println!("  Total System Power: {:.1}W", power);
        }

        if let Some(ref pressure) = chassis.thermal_pressure {
            println!("  Thermal Pressure: {}", pressure);
        }

        if let Some(inlet) = chassis.inlet_temperature {
            println!("  Inlet Temperature: {:.1}C", inlet);
        }

        if let Some(outlet) = chassis.outlet_temperature {
            println!("  Outlet Temperature: {:.1}C", outlet);
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
                "      Total: {:.1} GB, Used: {:.1} GB, Available: {:.1} GB ({:.1}% used)",
                total_gb, used_gb, available_gb, usage_percent
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
