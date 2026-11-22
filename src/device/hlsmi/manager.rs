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

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, TryLockError};

use once_cell::sync::Lazy;

use super::collector::DataCollector;
use super::config::HlsmiConfig;
use super::parser::GaudiMetricsData;
use super::store::MetricsStore;

/// Global singleton for HlsmiManager
static HLSMI_MANAGER: Lazy<Mutex<Option<Arc<HlsmiManager>>>> = Lazy::new(|| Mutex::new(None));

/// Guards the one-time panic hook installation.
static PANIC_HOOK: Once = Once::new();

/// Install, once per process, a panic hook that kills the running hl-smi
/// child so a panic does not leave it orphaned.
///
/// The hook finds the child through `HLSMI_MANAGER` at panic time rather
/// than capturing it, so repeated start/shutdown cycles neither grow the
/// hook chain nor keep old process handles alive. Every lock is taken with
/// `try_lock`: the panicking thread may already hold one.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            kill_running_child();
            previous(info);
        }));
    });
}

fn kill_running_child() {
    let manager = match HLSMI_MANAGER.try_lock() {
        Ok(guard) => guard.clone(),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
        Err(TryLockError::WouldBlock) => return,
    };
    if let Some(manager) = manager
        && let Ok(collector) = manager.collector.try_lock()
    {
        collector.kill_child_on_panic();
    }
}

/// Track if first data has been received
static FIRST_DATA_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Manages a long-running hl-smi process and its latest-snapshot store
pub struct HlsmiManager {
    collector: Mutex<DataCollector>,
}

impl HlsmiManager {
    /// Create a new HlsmiManager and start the hl-smi process
    fn new(interval_secs: u64) -> Result<Self, Box<dyn std::error::Error>> {
        let config = HlsmiConfig::with_interval_secs(interval_secs);
        let store = Arc::new(MetricsStore::new());
        let mut collector = DataCollector::new(config, store);
        install_panic_hook();

        // Start collection
        collector.start()?;

        Ok(Self {
            collector: Mutex::new(collector),
        })
    }

    /// Get the latest hl-smi data from the store
    fn get_latest_data_internal(&self) -> Result<GaudiMetricsData, Box<dyn std::error::Error>> {
        let collector = self.collector.lock().unwrap();
        let result = collector.get_latest_data();

        // Track first successful data retrieval
        if result.is_ok() && !FIRST_DATA_RECEIVED.load(Ordering::Relaxed) {
            FIRST_DATA_RECEIVED.store(true, Ordering::Relaxed);
        }

        result
    }

    /// Get latest data as Result (public API for backward compatibility)
    pub fn get_latest_data_result(&self) -> Result<GaudiMetricsData, Box<dyn std::error::Error>> {
        self.get_latest_data_internal()
    }
}

/// Initialize the global hl-smi manager
/// This should be called once at startup for systems with Intel Gaudi accelerators
pub fn initialize_hlsmi_manager(interval_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager_guard = HLSMI_MANAGER.lock().unwrap();
    if manager_guard.is_none() {
        let manager = HlsmiManager::new(interval_secs)?;
        *manager_guard = Some(Arc::new(manager));
    }
    Ok(())
}

/// Get the global hl-smi manager instance
pub fn get_hlsmi_manager() -> Option<Arc<HlsmiManager>> {
    HLSMI_MANAGER.lock().unwrap().clone()
}

/// Shutdown and cleanup the hl-smi manager
pub fn shutdown_hlsmi_manager() {
    // Drop the manager if it exists
    if let Some(_manager) = get_hlsmi_manager() {
        // Drop all Arc references
        {
            let mut manager_guard = HLSMI_MANAGER.lock().unwrap();
            *manager_guard = None;
        }

        // Reset first data flag
        FIRST_DATA_RECEIVED.store(false, Ordering::Relaxed);

        // The manager will be dropped when the last Arc reference is dropped
        // The Drop implementation in DataCollector will handle cleanup
    }
}

/// Check if hl-smi has received its first data
#[allow(dead_code)]
pub fn has_hlsmi_data() -> bool {
    FIRST_DATA_RECEIVED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_not_initialized() {
        // Ensure manager is not initialized
        shutdown_hlsmi_manager();

        // Manager should be None when not initialized
        let manager = get_hlsmi_manager();
        assert!(manager.is_none());
    }

    #[test]
    fn panic_hook_is_installed_once() {
        install_panic_hook();
        install_panic_hook();
        assert!(PANIC_HOOK.is_completed());
        // With no manager registered the hook has nothing to kill and must
        // return without blocking.
        kill_running_child();
    }
}
