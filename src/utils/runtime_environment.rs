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

use crate::common::config::AppConfig;
#[cfg(feature = "cli")]
use crossterm::style::Color;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum ContainerRuntime {
    Docker,
    Kubernetes,
    Podman,
    Containerd,
    Lxc,
    CriO,
    BackendAI,
    None,
}

impl ContainerRuntime {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContainerRuntime::Docker => "Docker",
            ContainerRuntime::Kubernetes => "Kubernetes",
            ContainerRuntime::Podman => "Podman",
            ContainerRuntime::Containerd => "containerd",
            ContainerRuntime::Lxc => "LXC/LXD",
            ContainerRuntime::CriO => "CRI-O",
            ContainerRuntime::BackendAI => "Backend.AI",
            ContainerRuntime::None => "None",
        }
    }

    #[cfg(feature = "cli")]
    pub fn brand_color(&self) -> Color {
        match self {
            ContainerRuntime::Docker => Color::Rgb {
                r: 36,
                g: 150,
                b: 237,
            }, // #2496ED
            ContainerRuntime::Kubernetes => Color::Rgb {
                r: 50,
                g: 108,
                b: 229,
            }, // #326CE5
            ContainerRuntime::Podman => Color::Rgb {
                r: 137,
                g: 44,
                b: 160,
            }, // #892CA0
            ContainerRuntime::BackendAI => Color::Rgb {
                r: 0,
                g: 212,
                b: 170,
            }, // #00D4AA
            ContainerRuntime::Containerd => Color::Rgb {
                r: 87,
                g: 89,
                b: 90,
            }, // #57595A
            ContainerRuntime::Lxc => Color::Rgb {
                r: 255,
                g: 102,
                b: 0,
            }, // #FF6600
            ContainerRuntime::CriO => Color::Rgb {
                r: 41,
                g: 66,
                b: 77,
            }, // #29424D
            ContainerRuntime::None => Color::DarkGrey,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub runtime: ContainerRuntime,
    pub container_id: Option<String>,
    pub pod_name: Option<String>,
    pub namespace: Option<String>,
}

impl ContainerInfo {
    pub fn is_containerized(&self) -> bool {
        self.runtime != ContainerRuntime::None
    }
}

/// Detects if the current process is running inside a container and identifies the runtime
pub fn detect_container_environment() -> ContainerInfo {
    // Check for Backend.AI
    if Path::new("/opt/kernel/libbaihook.so").exists() {
        return ContainerInfo {
            runtime: ContainerRuntime::BackendAI,
            container_id: env::var("BACKENDAI_KERNEL_ID")
                .or_else(|_| env::var("BACKEND_AI_KERNEL_ID"))
                .ok()
                .map(|id| id.chars().take(12).collect()),
            pod_name: None,
            namespace: None,
        };
    }

    // Check for Docker
    if Path::new("/.dockerenv").exists() {
        return ContainerInfo {
            runtime: ContainerRuntime::Docker,
            container_id: extract_container_id_from_cgroup("/docker/"),
            pod_name: None,
            namespace: None,
        };
    }

    // Check for Podman
    if Path::new("/run/.containerenv").exists()
        || env::var("container").unwrap_or_default() == "podman"
    {
        return ContainerInfo {
            runtime: ContainerRuntime::Podman,
            container_id: extract_container_id_from_cgroup("/machine.slice/"),
            pod_name: None,
            namespace: None,
        };
    }

    // Check for Kubernetes
    if env::var("KUBERNETES_SERVICE_HOST").is_ok()
        || Path::new("/var/run/secrets/kubernetes.io").exists()
    {
        let pod_name = env::var("HOSTNAME").ok();
        let namespace = read_k8s_namespace();

        return ContainerInfo {
            runtime: ContainerRuntime::Kubernetes,
            container_id: extract_container_id_from_cgroup("/kubepods/"),
            pod_name,
            namespace,
        };
    }

    // Check cgroup for other container runtimes
    if let Ok(cgroup_content) = fs::read_to_string("/proc/self/cgroup") {
        if cgroup_content.contains("/docker/") {
            return ContainerInfo {
                runtime: ContainerRuntime::Docker,
                container_id: extract_container_id_from_cgroup("/docker/"),
                pod_name: None,
                namespace: None,
            };
        } else if cgroup_content.contains("/kubepods/") {
            let pod_name = env::var("HOSTNAME").ok();
            let namespace = read_k8s_namespace();

            return ContainerInfo {
                runtime: ContainerRuntime::Kubernetes,
                container_id: extract_container_id_from_cgroup("/kubepods/"),
                pod_name,
                namespace,
            };
        } else if cgroup_content.contains("/containerd/") {
            return ContainerInfo {
                runtime: ContainerRuntime::Containerd,
                container_id: extract_container_id_from_cgroup("/containerd/"),
                pod_name: None,
                namespace: None,
            };
        } else if cgroup_content.contains("/lxc/") {
            return ContainerInfo {
                runtime: ContainerRuntime::Lxc,
                container_id: extract_container_id_from_cgroup("/lxc/"),
                pod_name: None,
                namespace: None,
            };
        } else if cgroup_content.contains("/crio-") {
            return ContainerInfo {
                runtime: ContainerRuntime::CriO,
                container_id: extract_container_id_from_cgroup("/crio-"),
                pod_name: None,
                namespace: None,
            };
        }
    }

    // Check for LXC environment variable
    if let Ok(environ) = fs::read_to_string("/proc/1/environ")
        && environ.contains("container=lxc")
    {
        return ContainerInfo {
            runtime: ContainerRuntime::Lxc,
            container_id: None,
            pod_name: None,
            namespace: None,
        };
    }

    // Not in a container
    ContainerInfo {
        runtime: ContainerRuntime::None,
        container_id: None,
        pod_name: None,
        namespace: None,
    }
}

/// Extract container ID from cgroup file
fn extract_container_id_from_cgroup(pattern: &str) -> Option<String> {
    if let Ok(cgroup_content) = fs::read_to_string("/proc/self/cgroup") {
        for line in cgroup_content.lines() {
            if let Some(pos) = line.find(pattern) {
                let id_start = pos + pattern.len();
                let remaining = &line[id_start..];

                // Extract the container ID (usually the next path component)
                let id = remaining
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("");

                if !id.is_empty() && id.len() >= 12 {
                    // Return first 12 characters of container ID
                    return Some(id.chars().take(12).collect());
                }
            }
        }
    }
    None
}

/// Read Kubernetes namespace from mounted secret
fn read_k8s_namespace() -> Option<String> {
    fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .ok()
        .map(|s| s.trim().to_string())
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum VirtualizationType {
    VMware,
    VirtualBox,
    Kvm,
    Qemu,
    HyperV,
    Xen,
    Aws,
    Gcp,
    Azure,
    DigitalOcean,
    Oracle,
    Parallels,
    None,
}

impl VirtualizationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            VirtualizationType::VMware => "VMware",
            VirtualizationType::VirtualBox => "VirtualBox",
            VirtualizationType::Kvm => "KVM",
            VirtualizationType::Qemu => "QEMU",
            VirtualizationType::HyperV => "Hyper-V",
            VirtualizationType::Xen => "Xen",
            VirtualizationType::Aws => "AWS EC2",
            VirtualizationType::Gcp => "Google Cloud",
            VirtualizationType::Azure => "Microsoft Azure",
            VirtualizationType::DigitalOcean => "DigitalOcean",
            VirtualizationType::Oracle => "Oracle Cloud",
            VirtualizationType::Parallels => "Parallels",
            VirtualizationType::None => "None",
        }
    }

    #[cfg(feature = "cli")]
    pub fn brand_color(&self) -> Color {
        match self {
            VirtualizationType::VMware => Color::Rgb {
                r: 113,
                g: 112,
                b: 116,
            }, // #717074
            VirtualizationType::VirtualBox => Color::Rgb {
                r: 24,
                g: 58,
                b: 92,
            }, // #183A5C
            VirtualizationType::Kvm => Color::Rgb {
                r: 255,
                g: 68,
                b: 68,
            }, // #FF4444 (Red Hat)
            VirtualizationType::Qemu => Color::Rgb {
                r: 255,
                g: 106,
                b: 0,
            }, // #FF6A00
            VirtualizationType::HyperV => Color::Rgb {
                r: 0,
                g: 188,
                b: 242,
            }, // #00BCF2
            VirtualizationType::Xen => Color::Rgb {
                r: 255,
                g: 143,
                b: 0,
            }, // #FF8F00
            VirtualizationType::Aws => Color::Rgb {
                r: 255,
                g: 153,
                b: 0,
            }, // #FF9900
            VirtualizationType::Gcp => Color::Rgb {
                r: 66,
                g: 133,
                b: 244,
            }, // #4285F4
            VirtualizationType::Azure => Color::Rgb {
                r: 0,
                g: 120,
                b: 212,
            }, // #0078D4
            VirtualizationType::DigitalOcean => Color::Rgb {
                r: 0,
                g: 105,
                b: 255,
            }, // #0069FF
            VirtualizationType::Oracle => Color::Rgb { r: 248, g: 0, b: 0 }, // #F80000
            VirtualizationType::Parallels => Color::Rgb {
                r: 223,
                g: 0,
                b: 56,
            }, // #DF0038
            VirtualizationType::None => Color::DarkGrey,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VirtualizationInfo {
    pub vm_type: VirtualizationType,
    pub hypervisor: Option<String>,
    pub is_virtual: bool,
}

/// Detects if the current process is running inside a virtual machine
pub fn detect_virtualization() -> VirtualizationInfo {
    // Try systemd-detect-virt first (most reliable if available)
    if let Ok(output) = Command::new("systemd-detect-virt").output()
        && output.status.success()
    {
        let virt_type = String::from_utf8_lossy(&output.stdout).trim().to_string();
        match virt_type.as_str() {
            "kvm" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::Kvm,
                    hypervisor: Some("KVM".to_string()),
                    is_virtual: true,
                };
            }
            "vmware" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::VMware,
                    hypervisor: Some("VMware".to_string()),
                    is_virtual: true,
                };
            }
            "oracle" | "virtualbox" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::VirtualBox,
                    hypervisor: Some("VirtualBox".to_string()),
                    is_virtual: true,
                };
            }
            "microsoft" | "hyperv" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::HyperV,
                    hypervisor: Some("Hyper-V".to_string()),
                    is_virtual: true,
                };
            }
            "xen" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::Xen,
                    hypervisor: Some("Xen".to_string()),
                    is_virtual: true,
                };
            }
            "qemu" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::Qemu,
                    hypervisor: Some("QEMU".to_string()),
                    is_virtual: true,
                };
            }
            "parallels" => {
                return VirtualizationInfo {
                    vm_type: VirtualizationType::Parallels,
                    hypervisor: Some("Parallels".to_string()),
                    is_virtual: true,
                };
            }
            "none" => {
                // Continue with other detection methods
            }
            _ if !virt_type.is_empty() => {
                // Unknown virtualization detected
                return VirtualizationInfo {
                    vm_type: VirtualizationType::None,
                    hypervisor: Some(virt_type),
                    is_virtual: true,
                };
            }
            _ => {}
        }
    }

    // Check DMI/SMBIOS information
    if let Ok(vendor) = fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
        let vendor = vendor.trim().to_lowercase();
        if vendor.contains("vmware") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VMware,
                hypervisor: Some("VMware".to_string()),
                is_virtual: true,
            };
        } else if vendor.contains("innotek") || vendor.contains("virtualbox") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VirtualBox,
                hypervisor: Some("VirtualBox".to_string()),
                is_virtual: true,
            };
        } else if vendor.contains("microsoft") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::HyperV,
                hypervisor: Some("Hyper-V".to_string()),
                is_virtual: true,
            };
        } else if vendor.contains("xen") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::Xen,
                hypervisor: Some("Xen".to_string()),
                is_virtual: true,
            };
        } else if vendor.contains("qemu") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::Qemu,
                hypervisor: Some("QEMU".to_string()),
                is_virtual: true,
            };
        } else if vendor.contains("parallels") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::Parallels,
                hypervisor: Some("Parallels".to_string()),
                is_virtual: true,
            };
        }
    }

    // Check product name
    if let Ok(product) = fs::read_to_string("/sys/class/dmi/id/product_name") {
        let product = product.trim().to_lowercase();
        if product.contains("vmware") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VMware,
                hypervisor: Some("VMware".to_string()),
                is_virtual: true,
            };
        } else if product.contains("virtualbox") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VirtualBox,
                hypervisor: Some("VirtualBox".to_string()),
                is_virtual: true,
            };
        } else if product.contains("virtual machine") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::HyperV,
                hypervisor: Some("Hyper-V".to_string()),
                is_virtual: true,
            };
        }
    }

    // Check for cloud providers
    if Path::new("/sys/hypervisor/uuid").exists()
        || env::var("AWS_EXECUTION_ENV").is_ok()
        || check_aws_metadata()
    {
        return VirtualizationInfo {
            vm_type: VirtualizationType::Aws,
            hypervisor: Some("AWS EC2".to_string()),
            is_virtual: true,
        };
    }

    // Check for GCP
    if let Ok(bios_vendor) = fs::read_to_string("/sys/class/dmi/id/bios_vendor")
        && bios_vendor.trim().to_lowercase().contains("google")
    {
        return VirtualizationInfo {
            vm_type: VirtualizationType::Gcp,
            hypervisor: Some("Google Cloud".to_string()),
            is_virtual: true,
        };
    }

    // Check for Azure
    if let Ok(chassis_asset_tag) = fs::read_to_string("/sys/class/dmi/id/chassis_asset_tag")
        && chassis_asset_tag.trim() == "7783-7084-3265-9085-8269-3286-77"
    {
        return VirtualizationInfo {
            vm_type: VirtualizationType::Azure,
            hypervisor: Some("Microsoft Azure".to_string()),
            is_virtual: true,
        };
    }

    // Check for DigitalOcean
    if let Ok(vendor) = fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        && vendor.trim().to_lowercase().contains("digitalocean")
    {
        return VirtualizationInfo {
            vm_type: VirtualizationType::DigitalOcean,
            hypervisor: Some("DigitalOcean".to_string()),
            is_virtual: true,
        };
    }

    // Check CPU flags for hypervisor
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo")
        && cpuinfo.contains("hypervisor")
    {
        return VirtualizationInfo {
            vm_type: VirtualizationType::None,
            hypervisor: Some("Unknown".to_string()),
            is_virtual: true,
        };
    }

    // Check for specific kernel modules
    if let Ok(modules) = fs::read_to_string("/proc/modules") {
        if modules.contains("vboxguest") || modules.contains("vboxsf") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VirtualBox,
                hypervisor: Some("VirtualBox".to_string()),
                is_virtual: true,
            };
        } else if modules.contains("vmw_balloon") || modules.contains("vmwgfx") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::VMware,
                hypervisor: Some("VMware".to_string()),
                is_virtual: true,
            };
        } else if modules.contains("hv_vmbus") || modules.contains("hv_storvsc") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::HyperV,
                hypervisor: Some("Hyper-V".to_string()),
                is_virtual: true,
            };
        } else if modules.contains("virtio") {
            return VirtualizationInfo {
                vm_type: VirtualizationType::Kvm,
                hypervisor: Some("KVM/QEMU".to_string()),
                is_virtual: true,
            };
        }
    }

    // Not in a VM
    VirtualizationInfo {
        vm_type: VirtualizationType::None,
        hypervisor: None,
        is_virtual: false,
    }
}

/// Check if AWS metadata service is accessible
fn check_aws_metadata() -> bool {
    // Make the AWS metadata check optional via environment variable
    if let Ok(val) = env::var("AWS_METADATA_CHECK_ENABLED")
        && (val == "0" || val.to_lowercase() == "false")
    {
        return false;
    }

    // Try to access AWS metadata service with very short timeout
    match Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "1",
            "--connect-timeout",
            "1",
            "http://169.254.169.254/latest/meta-data/instance-id",
        ])
        .output()
    {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeEnvironment {
    pub container: ContainerInfo,
    pub virtualization: VirtualizationInfo,
}

impl Default for RuntimeEnvironment {
    fn default() -> Self {
        Self {
            container: ContainerInfo {
                runtime: ContainerRuntime::None,
                container_id: None,
                pod_name: None,
                namespace: None,
            },
            virtualization: VirtualizationInfo {
                vm_type: VirtualizationType::None,
                hypervisor: None,
                is_virtual: false,
            },
        }
    }
}

impl RuntimeEnvironment {
    /// Detect both container and virtualization environments
    pub fn detect() -> Self {
        Self {
            container: detect_container_environment(),
            virtualization: detect_virtualization(),
        }
    }

    /// Get the display name and color for the runtime environment
    /// Prioritizes container environment over VM if both are detected
    #[cfg(feature = "cli")]
    pub fn display_info(&self) -> Option<(&str, Color)> {
        if self.container.is_containerized() {
            match self.container.runtime {
                ContainerRuntime::None => None,
                _ => Some((
                    self.container.runtime.as_str(),
                    self.container.runtime.brand_color(),
                )),
            }
        } else if self.virtualization.is_virtual {
            match self.virtualization.vm_type {
                VirtualizationType::None => None,
                _ => Some((
                    self.virtualization.vm_type.as_str(),
                    self.virtualization.vm_type.brand_color(),
                )),
            }
        } else {
            None
        }
    }

    /// Check if running in Backend.AI environment
    pub fn is_backend_ai(&self) -> bool {
        self.container.runtime == ContainerRuntime::BackendAI
    }

    /// Get Backend.AI cluster hosts from environment variable
    /// Returns a list of host URLs constructed from BACKENDAI_CLUSTER_HOSTS
    pub fn get_backend_ai_hosts(&self) -> Option<Vec<String>> {
        if !self.is_backend_ai() {
            return None;
        }

        // Try to get hosts from environment variable
        if let Ok(hosts_str) = env::var("BACKENDAI_CLUSTER_HOSTS") {
            let hosts: Vec<String> = hosts_str
                .split(',')
                .map(|host| {
                    let host = host.trim();
                    // If host doesn't have a scheme, prepend http://
                    if !host.starts_with("http://") && !host.starts_with("https://") {
                        format!("http://{host}:{}", AppConfig::BACKEND_AI_DEFAULT_PORT)
                    } else {
                        host.to_string()
                    }
                })
                .filter(|host| !host.is_empty())
                .collect();

            if !hosts.is_empty() {
                return Some(hosts);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_runtime_as_str() {
        assert_eq!(ContainerRuntime::Docker.as_str(), "Docker");
        assert_eq!(ContainerRuntime::Kubernetes.as_str(), "Kubernetes");
        assert_eq!(ContainerRuntime::Podman.as_str(), "Podman");
        assert_eq!(ContainerRuntime::None.as_str(), "None");
    }

    #[test]
    fn test_container_info_is_containerized() {
        let info = ContainerInfo {
            runtime: ContainerRuntime::Docker,
            container_id: Some("abc123".to_string()),
            pod_name: None,
            namespace: None,
        };
        assert!(info.is_containerized());

        let info = ContainerInfo {
            runtime: ContainerRuntime::None,
            container_id: None,
            pod_name: None,
            namespace: None,
        };
        assert!(!info.is_containerized());
    }
}
