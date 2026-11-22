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

/// Configuration for the hl-smi monitoring system
#[derive(Debug, Clone)]
pub struct HlsmiConfig {
    /// Interval in seconds for hl-smi collection
    pub interval_secs: u64,
    /// Monitoring thread sleep duration in seconds
    pub monitor_interval_secs: u64,
    /// Query fields for hl-smi command
    pub query_fields: Vec<String>,
}

impl Default for HlsmiConfig {
    fn default() -> Self {
        Self {
            interval_secs: 3, // Default 3 seconds
            monitor_interval_secs: 5,
            query_fields: vec![
                "index".to_string(),
                "uuid".to_string(),
                "name".to_string(),
                "driver_version".to_string(),
                "memory.total".to_string(),
                "memory.used".to_string(),
                "memory.free".to_string(),
                "power.draw".to_string(),
                "power.max".to_string(),
                "temperature.aip".to_string(),
                "utilization.aip".to_string(),
            ],
        }
    }
}

impl HlsmiConfig {
    /// Create a new configuration with the specified interval in seconds
    pub fn with_interval_secs(interval_secs: u64) -> Self {
        Self {
            interval_secs,
            ..Default::default()
        }
    }

    /// Validate a query field to prevent command injection
    fn validate_query_field(field: &str) -> bool {
        // Only allow alphanumeric characters, dots, and underscores
        field
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_')
            && field.len() <= 32
        // Reasonable length limit
    }

    /// Validate and sanitize the configuration
    fn validate(&self) -> Result<(), String> {
        // Validate interval is reasonable
        if self.interval_secs < 1 || self.interval_secs > 60 {
            return Err(format!(
                "Invalid interval: {}s. Must be between 1s and 60s",
                self.interval_secs
            ));
        }

        // Validate query fields
        for field in &self.query_fields {
            if !Self::validate_query_field(field) {
                return Err(format!(
                    "Invalid query field: '{field}'. Only alphanumeric, dot, and underscore allowed"
                ));
            }
        }

        Ok(())
    }

    /// Get the command-line arguments for hl-smi
    pub fn get_hlsmi_args(&self) -> Vec<String> {
        // Validate configuration before generating arguments
        if let Err(e) = self.validate() {
            eprintln!("hl-smi config validation failed: {e}");
            // Return safe defaults
            return vec![
                "-Q".to_string(),
                "index,uuid,name,driver_version,memory.total,memory.used,memory.free,power.draw,power.max,temperature.aip,utilization.aip".to_string(),
                "--format".to_string(),
                "csv,noheader".to_string(),
                "-l".to_string(),
                "3".to_string(),
            ];
        }

        vec![
            "-Q".to_string(),
            self.query_fields.join(","),
            "--format".to_string(),
            "csv,noheader".to_string(),
            "-l".to_string(),
            self.interval_secs.to_string(),
        ]
    }
}

/// Command types for the reader thread
#[derive(Debug)]
pub enum ReaderCommand {
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = HlsmiConfig::default();
        assert_eq!(config.interval_secs, 3);
        assert_eq!(config.monitor_interval_secs, 5);
        assert_eq!(config.query_fields.len(), 11);
    }

    #[test]
    fn test_config_with_interval() {
        let config = HlsmiConfig::with_interval_secs(5);
        assert_eq!(config.interval_secs, 5);
    }

    #[test]
    fn test_validate_query_field() {
        assert!(HlsmiConfig::validate_query_field("index"));
        assert!(HlsmiConfig::validate_query_field("memory.total"));
        assert!(HlsmiConfig::validate_query_field("utilization.aip"));
        assert!(!HlsmiConfig::validate_query_field("invalid;field"));
        assert!(!HlsmiConfig::validate_query_field("invalid|field"));
    }

    #[test]
    fn test_get_hlsmi_args() {
        let config = HlsmiConfig::default();
        let args = config.get_hlsmi_args();
        assert!(args.contains(&"-Q".to_string()));
        assert!(args.contains(&"--format".to_string()));
        assert!(args.contains(&"csv,noheader".to_string()));
        assert!(args.contains(&"-l".to_string()));
        assert!(args.contains(&"3".to_string()));
    }
}
