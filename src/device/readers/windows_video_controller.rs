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

//! Cached `Win32_VideoController` inventory for the Windows GPU readers.
//!
//! Every column the AMD and Intel readers use is static except `Status`,
//! which changes only on driver faults, so each reader keeps the rows for
//! [`CONTROLLER_TTL`] and retries a failed or empty query after
//! [`CONTROLLER_RETRY`] instead of querying WMI on every tick (issue #501).
//! The query runs on the shared WMI worker (`windows_wmi_worker`).

use crate::device::windows_wmi_worker::{CIMV2, with_wmi};
use crate::utils::cached_probe::CachedProbe;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a successful inventory is reused.
pub const CONTROLLER_TTL: Duration = Duration::from_secs(300);
/// How long a failed or empty inventory query waits before it is retried.
pub const CONTROLLER_RETRY: Duration = Duration::from_secs(30);

const CONTROLLER_QUERY: &str = "SELECT Name, AdapterRAM, DriverVersion, VideoProcessor, \
     PNPDeviceID, Status, AdapterDACType FROM Win32_VideoController";

/// One `Win32_VideoController` row.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Win32VideoController {
    pub name: Option<String>,
    /// `AdapterRAM` in bytes; a `uint32` in the WMI schema, so it saturates
    /// at 4 GB.
    pub adapter_r_a_m: Option<u64>,
    pub driver_version: Option<String>,
    pub video_processor: Option<String>,
    /// `PNPDeviceID`
    pub pnp_device_i_d: Option<String>,
    pub status: Option<String>,
    pub adapter_d_a_c_type: Option<String>,
}

type ControllerQuery = Box<dyn Fn() -> Option<Vec<Win32VideoController>> + Send + Sync>;

/// Per-reader cache of the controller rows.
pub struct VideoControllerCache {
    probe: Mutex<CachedProbe<Vec<Win32VideoController>>>,
    query: ControllerQuery,
}

impl Default for VideoControllerCache {
    fn default() -> Self {
        Self::with_query(query_video_controllers)
    }
}

impl VideoControllerCache {
    /// A cache backed by `query`; production uses the WMI query, tests a
    /// counting fake.
    pub fn with_query(
        query: impl Fn() -> Option<Vec<Win32VideoController>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            probe: Mutex::new(CachedProbe::new()),
            query: Box::new(query),
        }
    }

    /// Controller rows, from the cache when it is fresh. Empty when WMI has
    /// never answered.
    pub fn rows(&self) -> Vec<Win32VideoController> {
        self.rows_at(Instant::now())
    }

    fn rows_at(&self, now: Instant) -> Vec<Win32VideoController> {
        self.probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_try(now, Some(CONTROLLER_TTL), CONTROLLER_RETRY, || {
                (self.query)()
            })
            .unwrap_or_default()
    }
}

/// Query every video controller on the WMI worker. An empty answer counts
/// as a failure: a reader exists only because a controller was detected.
fn query_video_controllers() -> Option<Vec<Win32VideoController>> {
    with_wmi(|connections| {
        let queried = connections
            .get(CIMV2)?
            .raw_query::<Win32VideoController>(CONTROLLER_QUERY);
        if queried.is_err() {
            // The connection may be stale (winmgmt restarted): reconnect
            // on the next attempt instead of failing against it forever.
            connections.invalidate(CIMV2);
        }
        queried.ok()
    })
    .flatten()
    .filter(|rows| !rows.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A cache whose query counts its calls and answers `rows`.
    pub(crate) fn counting_cache(
        rows: Option<Vec<Win32VideoController>>,
    ) -> (VideoControllerCache, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let cache = VideoControllerCache::with_query(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            rows.clone()
        });
        (cache, calls)
    }

    pub(crate) fn controller(name: &str, pnp: &str) -> Win32VideoController {
        Win32VideoController {
            name: Some(name.to_string()),
            adapter_r_a_m: Some(4_293_918_720),
            driver_version: Some("31.0.101.5590".to_string()),
            video_processor: None,
            pnp_device_i_d: Some(pnp.to_string()),
            status: Some("OK".to_string()),
            adapter_d_a_c_type: None,
        }
    }

    #[test]
    fn rows_are_queried_once_inside_the_ttl() {
        let (cache, calls) = counting_cache(Some(vec![controller("GPU", "PCI\\1")]));
        let t0 = Instant::now();
        for tick in 0..10 {
            let rows = cache.rows_at(t0 + Duration::from_secs(tick * 2));
            assert_eq!(rows.len(), 1);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        cache.rows_at(t0 + CONTROLLER_TTL);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn failed_query_is_retried_after_thirty_seconds() {
        let (cache, calls) = counting_cache(None);
        let t0 = Instant::now();
        for tick in 0..15 {
            assert!(cache.rows_at(t0 + Duration::from_secs(tick * 2)).is_empty());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        cache.rows_at(t0 + CONTROLLER_RETRY);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
