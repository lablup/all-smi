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

use std::collections::HashMap;

/// Result type for mock generation operations
pub type MockResult<T> = Result<T, MockError>;

/// Errors that can occur during mock generation
#[derive(Debug)]
pub enum MockError {
    TemplateError(String),
    ConfigError(String),
    UnsupportedPlatform(String),
    RandomError(String),
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for MockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TemplateError(msg) => write!(f, "Template rendering failed: {msg}"),
            Self::ConfigError(msg) => write!(f, "Invalid configuration: {msg}"),
            Self::UnsupportedPlatform(msg) => write!(f, "Unsupported platform: {msg}"),
            Self::RandomError(msg) => write!(f, "Random generation failed: {msg}"),
            Self::Io(err) => write!(f, "IO error: {err}"),
            Self::Other(msg) => write!(f, "Other error: {msg}"),
        }
    }
}

impl std::error::Error for MockError {}

impl From<std::io::Error> for MockError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Platform types for mock generation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockPlatform {
    Nvidia,
    AppleSilicon,
    Jetson,
    AmdGpu,
    /// Intel **client** GPU family (Arc / Iris / Xe / integrated
    /// graphics, issue #244). Distinct from any Intel datacenter HPU
    /// path which is covered by other readers.
    IntelGpu,
    Tenstorrent,
    Rebellions,
    Furiosa,
    Custom(String),
}

/// Configuration for mock data generation
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Platform to simulate
    pub platform: MockPlatform,

    /// Number of devices to simulate
    pub device_count: usize,

    /// Node identifier
    pub node_name: String,

    /// Port number for this mock instance
    pub port: u16,

    /// Whether to include disk metrics
    pub include_disk_metrics: bool,

    /// Custom GPU name override
    pub gpu_name: Option<String>,

    /// Update interval for dynamic values
    pub update_interval: std::time::Duration,

    /// Random seed for reproducible data
    pub seed: Option<u64>,

    /// Additional platform-specific configuration
    pub extra_config: HashMap<String, String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            platform: MockPlatform::Nvidia,
            device_count: 8,
            node_name: "mock-node".to_string(),
            port: 9090,
            include_disk_metrics: true,
            gpu_name: None,
            update_interval: std::time::Duration::from_secs(3),
            seed: None,
            extra_config: HashMap::new(),
        }
    }
}

/// Generated mock data
#[derive(Debug, Clone)]
pub struct MockData {
    /// The generated response body
    pub response: String,

    /// Content type for the response
    pub content_type: String,

    /// Timestamp when data was generated
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Platform that generated this data
    pub platform: MockPlatform,
}

/// Trait for generating mock metrics data
pub trait MockGenerator: Send + Sync {
    /// Generate mock data based on configuration
    fn generate(&self, config: &MockConfig) -> MockResult<MockData>;

    /// Generate a template response (static structure)
    fn generate_template(&self, config: &MockConfig) -> MockResult<String>;

    /// Render dynamic values into the template
    fn render(&self, template: &str, config: &MockConfig) -> MockResult<String>;

    /// Get the platform this generator supports
    fn platform(&self) -> MockPlatform;

    /// Validate configuration for this generator
    fn validate_config(&self, config: &MockConfig) -> MockResult<()> {
        if config.platform != self.platform() {
            return Err(MockError::ConfigError(format!(
                "Platform mismatch: expected {:?}, got {:?}",
                self.platform(),
                config.platform
            )));
        }
        Ok(())
    }

    /// Get default configuration for this platform
    fn default_config(&self) -> MockConfig {
        MockConfig {
            platform: self.platform(),
            ..MockConfig::default()
        }
    }
}

/// Trait for generators that support dynamic updates
pub trait DynamicMockGenerator: MockGenerator {
    /// Update dynamic values (utilization, temperature, etc.)
    fn update_values(&mut self) -> MockResult<()>;

    /// Get current dynamic values
    fn get_current_values(&self) -> HashMap<String, f64>;

    /// Set a specific dynamic value
    fn set_value(&mut self, key: &str, value: f64) -> MockResult<()>;

    /// Reset all dynamic values to defaults
    fn reset_values(&mut self);
}

/// Trait for generators that support GPU process simulation
pub trait ProcessMockGenerator: MockGenerator {
    /// Add a mock process
    fn add_process(&mut self, pid: u32, name: String, memory_mb: u64) -> MockResult<()>;

    /// Remove a mock process
    fn remove_process(&mut self, pid: u32) -> MockResult<()>;

    /// Clear all mock processes
    fn clear_processes(&mut self);

    /// Get current mock processes
    fn get_processes(&self) -> Vec<MockProcess>;
}

/// Mock process information
#[derive(Debug, Clone)]
pub struct MockProcess {
    pub pid: u32,
    pub name: String,
    pub memory_mb: u64,
    pub gpu_index: usize,
    pub utilization: f64,
}

/// Factory for creating mock generators
pub trait MockGeneratorFactory {
    /// Create a generator for the specified platform
    fn create(&self, platform: MockPlatform) -> MockResult<Box<dyn MockGenerator>>;

    /// Create a dynamic generator for the specified platform
    fn create_dynamic(&self, platform: MockPlatform) -> MockResult<Box<dyn DynamicMockGenerator>>;

    /// Get list of supported platforms
    fn supported_platforms(&self) -> Vec<MockPlatform>;

    /// Check if a platform is supported
    fn is_supported(&self, platform: &MockPlatform) -> bool {
        self.supported_platforms().contains(platform)
    }
}

/// Builder for creating mock generators
pub trait MockGeneratorBuilder {
    type Generator: MockGenerator;

    /// Build the generator
    fn build(self) -> MockResult<Self::Generator>;

    /// Set the platform
    fn with_platform(self, platform: MockPlatform) -> Self;

    /// Set the configuration
    fn with_config(self, config: MockConfig) -> Self;

    /// Set the random seed
    fn with_seed(self, seed: u64) -> Self;
}

/// Trait for template engines used by mock generators
pub trait TemplateEngine: Send + Sync {
    /// Render a template with the given context
    fn render(&self, template: &str, context: &HashMap<String, String>) -> MockResult<String>;

    /// Load a template from a string
    fn load_template(&mut self, name: &str, template: &str) -> MockResult<()>;

    /// Check if a template exists
    fn has_template(&self, name: &str) -> bool;

    /// Get list of loaded templates
    fn list_templates(&self) -> Vec<String>;

    /// Clear all loaded templates
    fn clear_templates(&mut self);
}

/// Trait for value generators (random or deterministic)
pub trait ValueGenerator: Send + Sync {
    /// Generate a random float in range [min, max]
    fn generate_float(&mut self, min: f64, max: f64) -> f64;

    /// Generate a random integer in range [min, max]
    fn generate_int(&mut self, min: i64, max: i64) -> i64;

    /// Generate a random boolean with given probability
    fn generate_bool(&mut self, probability: f64) -> bool;

    /// Generate a random string of given length
    fn generate_string(&mut self, length: usize) -> String;

    /// Generate realistic GPU utilization value
    fn generate_gpu_utilization(&mut self) -> f64;

    /// Generate realistic temperature value
    fn generate_temperature(&mut self, base: f64, variance: f64) -> f64;

    /// Generate realistic memory usage
    fn generate_memory_usage(&mut self, total: u64) -> u64;

    /// Reset the generator (for deterministic generators)
    fn reset(&mut self);
}
