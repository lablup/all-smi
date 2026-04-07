// SPDX-FileCopyrightText: © 2025 Inureyes <inureyes@gmail.com>
// SPDX-License-Identifier: MIT

//! Common caching utilities and structures for AI accelerator readers.
//!
//! This module provides unified caching patterns and structures to ensure consistency
//! across all accelerator reader implementations (NVIDIA, AMD, Apple Silicon, Tenstorrent,
//! Rebellions, Furiosa, NVIDIA Jetson).
//!
//! # Design Principles
//!
//! - Use `OnceLock` for thread-safe, one-time initialization
//! - Standardize on `HashMap` for device collections
//! - Provide extensible `detail` field for platform-specific information
//! - Maintain ~95% reduction in redundant API calls (from PR #69)

use std::collections::HashMap;
use std::sync::OnceLock;

/// Maximum number of devices supported per platform
pub const MAX_DEVICES: usize = 256;

/// Common structure for static device information that doesn't change during runtime.
///
/// This structure is designed to be cached once per device and reused across all
/// subsequent metric collections. The `detail` field provides extensibility for
/// platform-specific information without breaking the common interface.
#[derive(Debug, Clone)]
pub struct DeviceStaticInfo {
    /// Device name (e.g., "NVIDIA RTX 4090", "Apple M2 Ultra", "Furiosa RNGD")
    pub name: String,

    /// Device UUID if available (not all devices have UUIDs)
    pub uuid: Option<String>,

    /// Extensible key-value storage for platform-specific details
    /// Examples:
    /// - NVIDIA: "CUDA Version" => "12.0", "PCIe Generation" => "4"
    /// - AMD: "ROCm Version" => "5.7", "VBIOS Version" => "xxx"
    /// - Apple: "Architecture" => "M2", "Die Count" => "2"
    pub detail: HashMap<String, String>,
}

impl DeviceStaticInfo {
    /// Creates a new DeviceStaticInfo instance
    #[allow(dead_code)]
    pub fn new(name: String, uuid: Option<String>) -> Self {
        Self {
            name,
            uuid,
            detail: HashMap::new(),
        }
    }

    /// Creates a new DeviceStaticInfo with pre-populated details
    pub fn with_details(
        name: String,
        uuid: Option<String>,
        detail: HashMap<String, String>,
    ) -> Self {
        Self { name, uuid, detail }
    }
}

/// Trait for platform-specific extensions to DeviceStaticInfo
#[allow(dead_code)]
pub trait StaticDeviceInfoExt {
    /// Creates a DeviceStaticInfo from platform-specific data
    fn from_platform_specific(data: &impl PlatformData) -> Self;

    /// Returns the library/driver name for this platform
    fn get_library_name() -> &'static str;

    /// Extracts the library/driver version if available
    fn get_library_version(&self) -> Option<String>;
}

/// Marker trait for platform-specific data structures
#[allow(dead_code)]
pub trait PlatformData {}

/// Helper trait for initializing static caches
#[allow(dead_code)]
pub trait CacheInitializer {
    /// Ensures the static cache is initialized, returning an error if initialization fails
    fn ensure_static_cache_initialized(&self) -> Result<(), Box<dyn std::error::Error>>;

    /// Validates the device count against MAX_DEVICES limit
    fn validate_device_count(count: usize) -> Result<(), Box<dyn std::error::Error>> {
        if count > MAX_DEVICES {
            return Err(format!("Too many devices detected: {count} (max: {MAX_DEVICES})").into());
        }
        Ok(())
    }
}

/// Helper for building detail HashMap with consistent patterns
#[derive(Debug, Default)]
pub struct DetailBuilder {
    detail: HashMap<String, String>,
}

impl DetailBuilder {
    /// Creates a new DetailBuilder
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a key-value pair, returns self for chaining
    pub fn insert(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.detail.insert(key.into(), value.into());
        self
    }

    /// Inserts a key-value pair only if the value is Some
    pub fn insert_optional(
        mut self,
        key: impl Into<String>,
        value: Option<impl Into<String>>,
    ) -> Self {
        if let Some(v) = value {
            self.detail.insert(key.into(), v.into());
        }
        self
    }

    /// Inserts library/driver information
    pub fn insert_lib_info(self, name: &str, version: Option<&str>) -> Self {
        let key = format!("{name} Version");
        if let Some(ver) = version {
            self.insert(key, ver)
        } else {
            self
        }
    }

    /// Inserts PCI information in a consistent format
    pub fn insert_pci_info(
        self,
        bus_id: Option<&str>,
        link_gen: Option<&str>,
        link_width: Option<&str>,
    ) -> Self {
        self.insert_optional("PCI Bus ID", bus_id)
            .insert_optional("PCIe Generation", link_gen)
            .insert_optional("PCIe Link Width", link_width)
    }

    /// Builds the final HashMap
    pub fn build(self) -> HashMap<String, String> {
        self.detail
    }
}

/// Helper function for getting or initializing a library version cache
///
/// This function ensures that expensive version extraction operations are performed
/// only once, caching the result for subsequent calls.
///
/// # Examples
///
/// ```ignore
/// static DRIVER_VERSION: OnceLock<Option<String>> = OnceLock::new();
///
/// let version = get_or_init_library_version(&DRIVER_VERSION, || {
///     // Expensive operation to extract driver version
///     extract_driver_version_from_system()
/// });
/// ```
#[allow(dead_code)]
pub fn get_or_init_library_version<F>(
    cache: &OnceLock<Option<String>>,
    extractor: F,
) -> Option<String>
where
    F: FnOnce() -> Option<String>,
{
    cache.get_or_init(extractor).clone()
}

/// Helper function for parsing PCI bus information from various formats
///
/// Handles common PCI bus ID formats:
/// - "0000:03:00.0" (standard format)
/// - "03:00.0" (short format)
/// - "PCI:0:3:0" (alternative format)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PciInfo {
    pub bus_id: String,
    pub domain: Option<u16>,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciInfo {
    /// Parses a PCI bus ID string into structured information
    #[allow(dead_code)]
    pub fn parse(pci_string: &str) -> Result<Self, String> {
        // Handle standard format: "0000:03:00.0" or "03:00.0"
        if pci_string.contains(':') && pci_string.contains('.') {
            let parts: Vec<&str> = pci_string.split(':').collect();

            let (domain, bus_str, dev_func) = if parts.len() == 3 {
                // Format: "0000:03:00.0"
                let domain = u16::from_str_radix(parts[0], 16)
                    .map_err(|_| format!("Invalid PCI domain: {}", parts[0]))?;
                (Some(domain), parts[1], parts[2])
            } else if parts.len() == 2 {
                // Format: "03:00.0"
                (None, parts[0], parts[1])
            } else {
                return Err(format!("Invalid PCI format: {pci_string}"));
            };

            let bus = u8::from_str_radix(bus_str, 16)
                .map_err(|_| format!("Invalid PCI bus: {bus_str}"))?;

            let dev_func_parts: Vec<&str> = dev_func.split('.').collect();
            if dev_func_parts.len() != 2 {
                return Err(format!("Invalid PCI device.function: {dev_func}"));
            }

            let device = u8::from_str_radix(dev_func_parts[0], 16)
                .map_err(|_| format!("Invalid PCI device: {}", dev_func_parts[0]))?;
            let function = u8::from_str_radix(dev_func_parts[1], 16)
                .map_err(|_| format!("Invalid PCI function: {}", dev_func_parts[1]))?;

            Ok(Self {
                bus_id: pci_string.to_string(),
                domain,
                bus,
                device,
                function,
            })
        } else {
            Err(format!("Unsupported PCI format: {pci_string}"))
        }
    }

    /// Formats the PCI info as a standard bus ID string
    #[allow(dead_code)]
    pub fn to_standard_format(&self) -> String {
        if let Some(domain) = self.domain {
            format!(
                "{domain:04x}:{:02x}:{:02x}.{}",
                self.bus, self.device, self.function
            )
        } else {
            format!("{:02x}:{:02x}.{}", self.bus, self.device, self.function)
        }
    }
}

/// Macro for building a detail HashMap with optional values
///
/// This macro simplifies the common pattern of building a HashMap where
/// some values might be None and should be skipped.
///
/// # Examples
///
/// ```ignore
/// let details = build_detail_map! {
///     "Name" => Some(device_name),
///     "UUID" => device_uuid,  // Option<String>
///     "Driver" => Some("nvidia-smi".to_string()),
///     "Temperature" => temp.map(|t| format!("{}°C", t)),
/// };
/// ```
#[macro_export]
macro_rules! build_detail_map {
    ($($key:expr_2021 => $value:expr_2021),* $(,)?) => {{
        let mut map = std::collections::HashMap::new();
        $(
            if let Some(val) = $value {
                map.insert($key.to_string(), val.to_string());
            }
        )*
        map
    }};
}

/// Macro for initializing a static device cache with consistent error handling
///
/// This macro provides a standard way to initialize OnceLock-based caches
/// across all accelerator readers.
///
/// # Examples
///
/// ```ignore
/// static DEVICE_CACHE: OnceLock<HashMap<u32, DeviceStaticInfo>> = OnceLock::new();
///
/// cache_device_static_info!(&DEVICE_CACHE, u32, || {
///     // Discovery function that returns Result<HashMap<u32, DeviceStaticInfo>>
///     discover_nvidia_devices()
/// });
/// ```
#[macro_export]
macro_rules! cache_device_static_info {
    ($cache:expr_2021, $key_type:ty, $discovery_fn:expr_2021) => {{
        use $crate::device::readers::common_cache::CacheInitializer;

        if $cache.get().is_some() {
            return Ok(());
        }

        let devices = $discovery_fn?;

        // Validate device count
        CacheInitializer::validate_device_count(devices.len())?;

        let cache_map = devices.into_iter().collect::<std::collections::HashMap<
            $key_type,
            $crate::device::readers::common_cache::DeviceStaticInfo,
        >>();

        let _ = $cache.set(cache_map);
        Ok(())
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_static_info_creation() {
        let info = DeviceStaticInfo::new("Test Device".to_string(), Some("uuid-123".to_string()));

        assert_eq!(info.name, "Test Device");
        assert_eq!(info.uuid, Some("uuid-123".to_string()));
        assert!(info.detail.is_empty());
    }

    #[test]
    fn test_detail_builder() {
        let details = DetailBuilder::new()
            .insert("Key1", "Value1")
            .insert_optional("Key2", Some("Value2"))
            .insert_optional("Key3", None::<String>)
            .insert_lib_info("CUDA", Some("12.0"))
            .insert_pci_info(Some("00:03.0"), Some("4"), Some("x16"))
            .build();

        assert_eq!(details.get("Key1"), Some(&"Value1".to_string()));
        assert_eq!(details.get("Key2"), Some(&"Value2".to_string()));
        assert_eq!(details.get("Key3"), None);
        assert_eq!(details.get("CUDA Version"), Some(&"12.0".to_string()));
        assert_eq!(details.get("PCI Bus ID"), Some(&"00:03.0".to_string()));
        assert_eq!(details.get("PCIe Generation"), Some(&"4".to_string()));
        assert_eq!(details.get("PCIe Link Width"), Some(&"x16".to_string()));
    }

    #[test]
    fn test_pci_info_parsing() {
        // Test standard format
        let pci = PciInfo::parse("0000:03:00.0").unwrap();
        assert_eq!(pci.domain, Some(0));
        assert_eq!(pci.bus, 3);
        assert_eq!(pci.device, 0);
        assert_eq!(pci.function, 0);
        assert_eq!(pci.to_standard_format(), "0000:03:00.0");

        // Test short format
        let pci = PciInfo::parse("03:00.1").unwrap();
        assert_eq!(pci.domain, None);
        assert_eq!(pci.bus, 3);
        assert_eq!(pci.device, 0);
        assert_eq!(pci.function, 1);
        assert_eq!(pci.to_standard_format(), "03:00.1");

        // Test invalid format
        assert!(PciInfo::parse("invalid").is_err());
    }

    #[test]
    fn test_build_detail_map_macro() {
        let name = Some("Device".to_string());
        let uuid: Option<String> = None;
        let temp = Some(75);

        let details = build_detail_map! {
            "Name" => name,
            "UUID" => uuid,
            "Temperature" => temp.map(|t| format!("{t}°C")),
        };

        assert_eq!(details.get("Name"), Some(&"Device".to_string()));
        assert_eq!(details.get("UUID"), None);
        assert_eq!(details.get("Temperature"), Some(&"75°C".to_string()));
    }
}
