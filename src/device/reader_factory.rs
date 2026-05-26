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

use crate::device::{
    platform_detection::{
        get_os_type, has_furiosa, has_gaudi, has_intel_gpu, has_nvidia, has_rebellions, is_jetson,
    },
    readers::{furiosa, gaudi, nvidia, nvidia_jetson, rebellions},
    traits::{CpuReader, GpuReader, MemoryReader},
};

#[cfg(target_os = "linux")]
use crate::device::platform_detection::{has_google_tpu, has_tenstorrent};

#[cfg(target_os = "linux")]
use crate::device::readers::{google_tpu, tenstorrent};

#[cfg(target_os = "macos")]
use crate::device::{cpu_macos, memory_macos, platform_detection::is_apple_silicon};

#[cfg(target_os = "macos")]
use crate::device::readers::apple_silicon_native;

#[cfg(target_os = "linux")]
use crate::device::{cpu_linux, memory_linux};

#[cfg(target_os = "windows")]
use crate::device::{cpu_windows, memory_windows};

#[cfg(target_os = "windows")]
use crate::device::readers::amd_windows;

#[cfg(target_os = "linux")]
use crate::device::readers::intel_gpu_linux;

#[cfg(target_os = "windows")]
use crate::device::readers::intel_gpu_windows;

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use crate::device::platform_detection::has_amd;

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use crate::device::readers::amd;

pub fn get_gpu_readers() -> Vec<Box<dyn GpuReader>> {
    let mut readers: Vec<Box<dyn GpuReader>> = Vec::new();

    // Check if GPU detection should be skipped (useful for containers)
    if std::env::var("SKIP_GPU_DETECTION").is_ok() || std::env::var("NO_GPU").is_ok() {
        eprintln!("GPU detection skipped (SKIP_GPU_DETECTION or NO_GPU environment variable set)");
        return readers;
    }

    let os_type = get_os_type();

    match os_type {
        "linux" => {
            // Only create NVIDIA reader if we actually have NVIDIA GPUs
            if is_jetson() && has_nvidia() {
                readers.push(Box::new(nvidia_jetson::NvidiaJetsonGpuReader::new()));
            } else if has_nvidia() && !is_jetson() {
                readers.push(Box::new(nvidia::NvidiaGpuReader::new()));
            }

            // Check for Furiosa NPU support
            if has_furiosa() {
                readers.push(Box::new(furiosa::FuriosaNpuReader::new()));
            }

            // Check for Tenstorrent NPU support
            #[cfg(target_os = "linux")]
            if has_tenstorrent() {
                readers.push(Box::new(tenstorrent::TenstorrentReader::new()));
            }

            // Check for Rebellions NPU support
            if has_rebellions() {
                readers.push(Box::new(rebellions::RebellionsNpuReader::new()));
            }

            // Check for Intel Gaudi NPU support
            if has_gaudi() {
                readers.push(Box::new(gaudi::GaudiNpuReader::new()));
            }

            // Check for Google TPU support
            #[cfg(target_os = "linux")]
            if has_google_tpu() {
                readers.push(Box::new(google_tpu::GoogleTpuReader::new()));
            }

            // Check for AMD GPU support (glibc only, not musl))
            #[cfg(all(target_os = "linux", not(target_env = "musl")))]
            if has_amd() {
                readers.push(Box::new(amd::AmdGpuReader::new()));
            }

            // Check for Intel client GPU (Arc / Iris / Xe). Unlike
            // `libamdgpu_top`, the Intel sysfs reader has no glibc
            // constraint and works on both glibc and musl targets.
            #[cfg(target_os = "linux")]
            if has_intel_gpu() {
                readers.push(Box::new(intel_gpu_linux::IntelGpuReader::new()));
            }
        }
        "macos" => {
            #[cfg(target_os = "macos")]
            if is_apple_silicon() {
                // Use native APIs (no sudo required)
                readers.push(Box::new(
                    apple_silicon_native::AppleSiliconNativeGpuReader::new(),
                ));
            }
        }
        "windows" => {
            #[cfg(target_os = "windows")]
            {
                // Check for NVIDIA GPU on Windows
                if has_nvidia() {
                    readers.push(Box::new(nvidia::NvidiaGpuReader::new()));
                }

                // Check for AMD GPU on Windows (including APU)
                if amd_windows::has_amd_gpu_windows() {
                    readers.push(Box::new(amd_windows::AmdWindowsGpuReader::new()));
                }

                // Check for Intel client GPU (Arc / Iris / Xe) on
                // Windows via WMI.
                if intel_gpu_windows::has_intel_gpu_windows() {
                    readers.push(Box::new(intel_gpu_windows::IntelWindowsGpuReader::new()));
                }
            }
        }
        _ => println!("Unsupported OS type: {os_type}"),
    }

    readers
}

#[allow(unused_mut)]
pub fn get_cpu_readers() -> Vec<Box<dyn CpuReader>> {
    let mut readers: Vec<Box<dyn CpuReader>> = Vec::new();
    let os_type = get_os_type();

    match os_type {
        "linux" => {
            #[cfg(target_os = "linux")]
            readers.push(Box::new(cpu_linux::LinuxCpuReader::new()));
        }
        "macos" => {
            #[cfg(target_os = "macos")]
            readers.push(Box::new(cpu_macos::MacOsCpuReader::new()));
        }
        "windows" => {
            #[cfg(target_os = "windows")]
            readers.push(Box::new(cpu_windows::WindowsCpuReader::new()));
        }
        _ => println!("CPU monitoring not supported for OS type: {os_type}"),
    }

    readers
}

#[allow(unused_mut)]
pub fn get_memory_readers() -> Vec<Box<dyn MemoryReader>> {
    let mut readers: Vec<Box<dyn MemoryReader>> = Vec::new();
    let os_type = get_os_type();

    match os_type {
        "linux" => {
            #[cfg(target_os = "linux")]
            readers.push(Box::new(memory_linux::LinuxMemoryReader::new()));
        }
        "macos" => {
            #[cfg(target_os = "macos")]
            readers.push(Box::new(memory_macos::MacOsMemoryReader::new()));
        }
        "windows" => {
            #[cfg(target_os = "windows")]
            readers.push(Box::new(memory_windows::WindowsMemoryReader::new()));
        }
        _ => println!("Memory monitoring not supported for OS type: {os_type}"),
    }

    readers
}
