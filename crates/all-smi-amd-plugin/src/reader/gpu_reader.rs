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

impl GpuReader for AmdGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let mut gpu_info = Vec::new();

        for device in &self.devices {
            // Get cached static device information (fetched only once)
            let static_info = self.get_device_static_info(device);
            let mut detail = static_info.detail.clone();
            let device_name = static_info.name.clone();

            // Get device info for dynamic metrics only
            let ext_info = match device.device_handle.device_info() {
                Ok(info) => info,
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to get device info for AMD GPU {}: {e}",
                        device.device_path.pci
                    );
                    continue; // Skip this GPU if we can't get device info
                }
            };

            // Update the VramUsage from the driver (following libamdgpu-top pattern)
            // SAFETY: We handle mutex poisoning by recreating the VramUsage from fresh memory_info
            let memory_info = {
                let vram_usage_result = device.vram_usage.lock();

                match vram_usage_result {
                    Ok(mut vram_usage) => {
                        // Normal path: update and read
                        vram_usage.update_usage(&device.device_handle);
                        vram_usage.update_usable_heap_size(&device.device_handle);
                        vram_usage.0 // VramUsage is a tuple struct wrapping drm_amdgpu_memory_info
                    }
                    Err(poisoned) => {
                        // Mutex was poisoned - recover by getting fresh memory info
                        // This prevents denial of service from panics in other threads
                        eprintln!(
                            "Warning: VramUsage mutex was poisoned for device {}, recovering...",
                            device.device_path.pci
                        );

                        // Try to get fresh memory info from the device
                        match device.device_handle.memory_info() {
                            Ok(fresh_memory_info) => {
                                // Attempt to recover the poisoned mutex safely
                                // into_inner() can theoretically panic if the mutex is in an
                                // inconsistent state, though this is extremely rare with modern
                                // standard library implementations
                                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    poisoned.into_inner()
                                })) {
                                    Ok(mut guard) => {
                                        // Successfully recovered the guard
                                        *guard = VramUsage::new(&fresh_memory_info);
                                        guard.update_usage(&device.device_handle);
                                        guard.update_usable_heap_size(&device.device_handle);
                                        guard.0
                                    }
                                    Err(_) => {
                                        // Recovery failed - skip this GPU
                                        eprintln!(
                                            "Critical: Failed to recover poisoned mutex for device {}, skipping",
                                            device.device_path.pci
                                        );
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to get fresh memory info during recovery: {e}");
                                continue; // Skip this GPU if we can't recover
                            }
                        }
                    }
                }
            };

            // Get dynamic sensor information
            let sensors = libamdgpu_top::stat::Sensors::new(
                &device.device_handle,
                &device.device_path.pci,
                &ext_info,
            );

            // Add dynamic sensor data to details
            //
            // The tachometer reading is published twice on purpose: once as
            // the typed `GpuInfo::fan_speed_rpm` field that the TUI and the
            // Prometheus exporter read, and once as the `Fan Speed` detail
            // string that snapshots and the cross-reader overwrite guard in
            // `intel_gpu_level_zero::apply_fan` still depend on. Both come
            // from the same `sensors.fan_rpm` value so they cannot disagree.
            let mut fan_speed_rpm = None;
            if let Some(ref sensors) = sensors {
                if let Some(link) = sensors.current_link {
                    detail.insert(
                        "Current Link".to_string(),
                        format!("Gen{} x{}", link.r#gen, link.width),
                    );
                }
                if let Some(fan) = clamp_fan_rpm(sensors.fan_rpm) {
                    fan_speed_rpm = Some(fan);
                    detail.insert("Fan Speed".to_string(), format!("{fan} RPM"));
                }
                if let Some(mclk) = sensors.mclk {
                    detail.insert("Memory Clock".to_string(), format!("{mclk} MHz"));
                }
            }

            let mut utilization = 0.0;
            let mut power_consumption = 0.0;
            let mut temperature: u32 = 0;
            let mut frequency: u32 = 0;

            // Try to get metrics from GpuMetrics first with validation
            if let Ok(metrics) = GpuMetrics::get_from_sysfs_path(&device.device_path.sysfs_path) {
                if let Some(gfx_activity) = metrics.get_average_gfx_activity() {
                    // Validate utilization is within reasonable bounds
                    utilization = (gfx_activity as f64).clamp(0.0, MAX_GPU_UTILIZATION);
                }
                if let Some(power) = metrics.get_average_socket_power() {
                    // Validate power consumption
                    let watts = power as f64 / 1000.0; // Convert mW to W
                    power_consumption = watts.clamp(0.0, MAX_GPU_POWER_WATTS);
                }
                if let Some(temp) = metrics.get_temperature_edge() {
                    // Validate temperature
                    temperature = (temp as u32).min(MAX_GPU_TEMP_CELSIUS);
                }
                if let Some(freq) = metrics.get_current_gfxclk() {
                    // Validate frequency
                    frequency = (freq as u32).min(MAX_GPU_FREQ_MHZ);
                }
            }

            // Fallback to sensors if metrics failed or missing (with validation)
            if let Some(ref s) = sensors {
                if utilization == 0.0 {
                    // Approximate utilization from load if available, or leave 0
                    // libamdgpu_top doesn't expose a simple "gpu load" sensor easily without GpuMetrics or fdinfo
                }
                if power_consumption == 0.0 {
                    if let Some(ref p) = s.average_power {
                        let watts = p.value as f64 / 1000.0; // Convert mW to W
                        power_consumption = watts.clamp(0.0, MAX_GPU_POWER_WATTS);
                    } else if let Some(ref p) = s.input_power {
                        let watts = p.value as f64 / 1000.0; // Convert mW to W
                        power_consumption = watts.clamp(0.0, MAX_GPU_POWER_WATTS);
                    }
                }
                if temperature == 0
                    && let Some(ref t) = s.edge_temp
                {
                    temperature = (t.current as u32).min(MAX_GPU_TEMP_CELSIUS);
                }
                if frequency == 0
                    && let Some(clk) = s.sclk
                {
                    frequency = clk.min(MAX_GPU_FREQ_MHZ);
                }
            }

            // Use memory_info from VramUsage (already updated above)
            // The update_usable_heap_size() call updates total_heap_size from vram_gtt_info()
            // but we do it once per update cycle, not repeated queries

            // Get VRAM size - try multiple sources in order with validation
            // Current max is MI325X with 288GB, but we allow headroom for future models
            // Use saturating operations to prevent any possibility of overflow
            let total_memory = if memory_info.vram.total_heap_size > 0 {
                memory_info.vram.total_heap_size.min(MAX_GPU_MEMORY_BYTES)
            } else if memory_info.vram.usable_heap_size > 0 {
                memory_info.vram.usable_heap_size.min(MAX_GPU_MEMORY_BYTES)
            } else {
                0
            };

            // Validate used memory doesn't exceed total - use saturating_sub to prevent underflow
            // in case of driver reporting incorrect values
            let used_memory = memory_info.vram.heap_usage.min(total_memory);

            let info = GpuInfo {
                uuid: format!("GPU-{}", device.device_path.pci), // AMD doesn't have UUIDs like NVIDIA, use PCI
                time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                name: device_name, // Use cached device name
                device_type: "GPU".to_string(),
                host_id: get_hostname(),
                hostname: get_hostname(),
                instance: get_hostname(),
                utilization,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature,
                used_memory,
                total_memory,
                frequency,
                power_consumption,
                gpu_core_count: None,
                // AMD GPUs surface temperature through libamdgpu_top; NVML
                // thermal-threshold and P-state APIs do not apply here.
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                fan_speed_rpm,
                // NVIDIA-specific hardware details (NUMA, GSP firmware,
                // NvLink, GPM) do not apply to AMD — leave them at the
                // "unavailable" defaults so consumers render them as
                // missing rather than zero.
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
                detail,
            };
            gpu_info.push(info);
        }

        gpu_info
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        use std::collections::{HashMap, HashSet};

        let mut process_info_list = Vec::new();

        // Get process list once for fdinfo parsing
        let proc_list = stat::get_process_list();

        // Collect all GPU process data in a single pass
        struct GpuProcessData {
            device_id: usize,
            device_uuid: String,
            pid: u32,
            name: String,
            vram_usage_kib: u64,
            gtt_usage_kib: u64,
        }

        let mut gpu_processes = Vec::new();
        let mut gpu_pids = HashSet::new();

        // Single pass: collect all GPU process data
        for (device_idx, device) in self.devices.iter().enumerate() {
            // Build process index for this device
            let mut proc_index: Vec<ProcInfo> = Vec::new();
            stat::update_index_by_all_proc(
                &mut proc_index,
                &[&device.device_path.render, &device.device_path.card],
                &proc_list,
            );

            // Get fdinfo usage for all processes
            let mut fdinfo = FdInfoStat::default();
            fdinfo.update_proc_usage(&proc_index);

            // Collect process data
            for proc_usage in fdinfo.proc_usage {
                let vram_usage_kib = proc_usage.usage.vram_usage;
                let gtt_usage_kib = proc_usage.usage.gtt_usage;

                // Include process if it uses VRAM or GTT (GPU memory)
                if vram_usage_kib > 0 || gtt_usage_kib > 0 {
                    let pid = proc_usage.pid as u32;
                    gpu_pids.insert(pid);

                    gpu_processes.push(GpuProcessData {
                        device_id: device_idx,
                        device_uuid: format!("GPU-{}", device.device_path.pci),
                        pid,
                        name: proc_usage.name,
                        vram_usage_kib,
                        gtt_usage_kib,
                    });
                }
            }
        }

        // Get system process information once for all GPU processes
        // OPTIMIZATION: Use minimal refresh instead of refresh_all() which is extremely expensive
        // We only need CPU, memory, and basic process info for GPU processes
        use all_smi::utils::with_global_system;
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
        let system_processes = with_global_system(|system| {
            let refresh_kind = ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_user(UpdateKind::OnlyIfNotSet);
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            all_smi::device::process_list::get_all_processes(system, &gpu_pids)
        });
        let process_map: HashMap<u32, _> = system_processes.iter().map(|p| (p.pid, p)).collect();

        // Build final ProcessInfo list efficiently
        for gpu_proc in gpu_processes {
            // Convert to bytes and prioritize VRAM, fallback to GTT
            let gpu_memory_bytes = if gpu_proc.vram_usage_kib > 0 {
                gpu_proc.vram_usage_kib * 1024
            } else {
                gpu_proc.gtt_usage_kib * 1024
            };

            // Get system process info or use defaults
            let sys_proc = process_map.get(&gpu_proc.pid);

            let process_info = ProcessInfo {
                device_id: gpu_proc.device_id,
                device_uuid: gpu_proc.device_uuid,
                pid: gpu_proc.pid,
                process_name: gpu_proc.name,
                used_memory: gpu_memory_bytes,
                cpu_percent: sys_proc.map(|p| p.cpu_percent).unwrap_or(0.0),
                memory_percent: sys_proc.map(|p| p.memory_percent).unwrap_or(0.0),
                memory_rss: sys_proc.map(|p| p.memory_rss).unwrap_or(0),
                memory_vms: sys_proc.map(|p| p.memory_vms).unwrap_or(0),
                user: sys_proc.map(|p| p.user.clone()).unwrap_or_default(),
                state: sys_proc.map(|p| p.state.clone()).unwrap_or_default(),
                start_time: sys_proc.map(|p| p.start_time.clone()).unwrap_or_default(),
                cpu_time: sys_proc.map(|p| p.cpu_time).unwrap_or(0),
                command: sys_proc.map(|p| p.command.clone()).unwrap_or_default(),
                ppid: sys_proc.map(|p| p.ppid).unwrap_or(0),
                threads: sys_proc.map(|p| p.threads).unwrap_or(0),
                uses_gpu: true,
                priority: sys_proc.map(|p| p.priority).unwrap_or(0),
                nice_value: sys_proc.map(|p| p.nice_value).unwrap_or(0),
                gpu_utilization: 0.0, // fdinfo doesn't directly provide this per-process
            };

            process_info_list.push(process_info);
        }

        process_info_list
    }
}
