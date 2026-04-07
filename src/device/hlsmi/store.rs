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

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::parser::{GaudiMetricsData, parse_hlsmi_output};

/// Stores hl-smi data in a circular buffer
pub struct MetricsStore {
    /// Circular buffer storing complete hl-smi CSV sections
    data_buffer: Arc<Mutex<VecDeque<String>>>,
    /// Cache of the last parsed data
    last_data: Arc<Mutex<Option<GaudiMetricsData>>>,
}

impl MetricsStore {
    /// Create a new MetricsStore with the specified capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            data_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            last_data: Arc::new(Mutex::new(None)),
        }
    }

    /// Add a new section to the buffer (used in tests)
    #[cfg(test)]
    pub fn add_section(&self, section: String, capacity: usize) {
        let mut buffer = self.data_buffer.lock().unwrap();
        if buffer.len() >= capacity {
            buffer.pop_front(); // Remove oldest
        }
        buffer.push_back(section);
    }

    /// Get the buffer for direct access (used by collector)
    pub fn get_buffer(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.data_buffer.clone()
    }

    /// Get the latest hl-smi data from the circular buffer
    pub fn get_latest_data(&self) -> Result<GaudiMetricsData, Box<dyn std::error::Error>> {
        // Get the most recent complete section from the buffer
        let latest_section = {
            let buffer = self.data_buffer.lock().unwrap();
            buffer.back().cloned()
        };

        if let Some(section) = latest_section {
            // Parse the data
            if let Ok(data) = parse_hlsmi_output(&section) {
                // Cache the data
                let mut last_data = self.last_data.lock().unwrap();
                *last_data = Some(data.clone());
                return Ok(data);
            }
        }

        // If we can't read fresh data, return cached data if available
        if let Some(cached) = self.last_data.lock().unwrap().clone() {
            return Ok(cached);
        }

        Err("No hl-smi data available".into())
    }

    /// Clear all stored data
    pub fn clear(&self) {
        let mut buffer = self.data_buffer.lock().unwrap();
        buffer.clear();

        let mut last_data = self.last_data.lock().unwrap();
        *last_data = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_overflow_protection() {
        let capacity = 5;
        let store = MetricsStore::new(capacity);

        // Add more sections than capacity
        for i in 0..10 {
            let section = format!(
                "0, UUID-{i}, HL-325L, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %"
            );
            store.add_section(section, capacity);
        }

        // Verify buffer size is maintained at limit
        let buffer = store.data_buffer.lock().unwrap();
        assert_eq!(buffer.len(), capacity);
        assert!(buffer.back().unwrap().contains("UUID-9"));
        assert!(buffer.front().unwrap().contains("UUID-5"));
    }

    #[test]
    fn test_concurrent_buffer_access() {
        use std::thread;
        use std::time::Duration;

        let capacity = 100;
        let store = Arc::new(MetricsStore::new(capacity));
        let mut handles = vec![];

        // Spawn multiple threads accessing the buffer
        for i in 0..5 {
            let store_clone = store.clone();
            let handle = thread::spawn(move || {
                for j in 0..20 {
                    let section = format!(
                        "0, UUID-{i}-{j}, HL-325L, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %"
                    );
                    store_clone.add_section(section, capacity);
                    thread::sleep(Duration::from_micros(100));
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all items were added
        let buffer = store.data_buffer.lock().unwrap();
        assert_eq!(buffer.len(), capacity); // 5 threads * 20 items = 100
    }

    #[test]
    fn test_get_latest_data() {
        let store = MetricsStore::new(10);
        let section = "0, 01P4-HL3090A0-18-U4V193-22-07-00, HL-325L, 1.22.1-97ec1a4, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %\n\
                       1, 01P4-HL3090A0-18-U4V298-03-04-04, HL-325L, 1.22.1-97ec1a4, 131072 MiB, 672 MiB, 130400 MiB, 230 W, 850 W, 39 C, 0 %";
        store.add_section(section.to_string(), 10);

        let result = store.get_latest_data();
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.devices.len(), 2);
        assert_eq!(data.devices[0].index, 0);
        assert_eq!(data.devices[0].driver_version, "1.22.1");
        assert_eq!(data.devices[1].index, 1);
    }
}
