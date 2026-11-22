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

//! One process-wide owner for every WMI connection (issue #501).
//!
//! `WMIConnection` is `!Send`, so the readers used to keep connections in
//! `thread_local!` cells. View mode runs each reader pass on whichever tokio
//! blocking-pool thread is free, so every pool thread built its own set and
//! a reaped thread took its set with it. Instead, one dedicated thread owns
//! the connections, created lazily per namespace and kept, and readers send
//! it closures through [`with_wmi`]. A caller waits at most
//! [`REPLY_TIMEOUT`], so a wedged WMI provider costs a missing reading, never
//! a hung collection tick.
//!
//! [`ConnectionCache`] is generic over the connection type so its caching
//! and invalidation are unit-tested on every runner with a counting factory.

use std::collections::HashMap;

/// Opens a connection to a namespace; `None` when it cannot right now.
type ConnectionFactory<C> = Box<dyn FnMut(&str) -> Option<C>>;

/// Lazily created connections keyed by WMI namespace path.
///
/// Connections are boxed so a reference handed out by [`Self::get`] points
/// at the same connection for as long as it stays cached.
pub struct ConnectionCache<C> {
    connections: HashMap<&'static str, Box<C>>,
    factory: ConnectionFactory<C>,
}

impl<C> ConnectionCache<C> {
    /// A cache that opens connections with `factory`; `None` means the
    /// namespace could not be connected right now and is retried next time.
    pub fn with_factory(factory: impl FnMut(&str) -> Option<C> + 'static) -> Self {
        Self {
            connections: HashMap::new(),
            factory: Box::new(factory),
        }
    }

    /// The cached connection for `namespace`, connecting on first use.
    pub fn get(&mut self, namespace: &'static str) -> Option<&C> {
        if !self.connections.contains_key(namespace) {
            let connection = (self.factory)(namespace)?;
            self.connections.insert(namespace, Box::new(connection));
        }
        self.connections
            .get(namespace)
            .map(|connection| &**connection)
    }

    /// Drop the cached connection for `namespace` after a failed query, so
    /// the next [`Self::get`] reconnects (the provider may have restarted).
    pub fn invalidate(&mut self, namespace: &str) {
        self.connections.remove(namespace);
    }
}

#[cfg(target_os = "windows")]
pub use worker::*;

#[cfg(target_os = "windows")]
mod worker {
    use super::ConnectionCache;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::OnceLock;
    use std::sync::mpsc::{self, Sender};
    use std::thread;
    use std::time::Duration;
    use wmi::WMIConnection;

    /// `ROOT\CIMV2`: Win32 classes and performance counters.
    pub const CIMV2: &str = "ROOT\\CIMV2";
    /// `root\WMI`: `MSAcpi_ThermalZoneTemperature`.
    pub const ROOT_WMI: &str = "root\\WMI";
    /// `root\Intel`: Intel chipset thermal classes.
    pub const INTEL: &str = "root\\Intel";
    /// LibreHardwareMonitor's namespace.
    pub const LIBRE_HARDWARE_MONITOR: &str = "root\\LibreHardwareMonitor";
    /// OpenHardwareMonitor's namespace (older LibreHardwareMonitor builds).
    pub const OPEN_HARDWARE_MONITOR: &str = "root\\OpenHardwareMonitor";

    /// How long [`with_wmi`] waits for the worker to answer.
    pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

    /// The worker's connections, one per namespace.
    pub type WmiConnections = ConnectionCache<WMIConnection>;

    type Job = Box<dyn FnOnce(&mut WmiConnections) + Send>;

    /// Job queue of the worker thread, started on first use. `None` when the
    /// thread could not be spawned.
    static WORKER: OnceLock<Option<Sender<Job>>> = OnceLock::new();

    fn worker() -> Option<&'static Sender<Job>> {
        WORKER
            .get_or_init(|| {
                let (tx, rx) = mpsc::channel::<Job>();
                thread::Builder::new()
                    .name("all-smi-wmi".to_string())
                    .spawn(move || {
                        let mut connections = WmiConnections::with_factory(|namespace| {
                            WMIConnection::with_namespace_path(namespace).ok()
                        });
                        for job in rx {
                            // A panicking job must not take the worker, and
                            // with it every later WMI reading, down too.
                            let _ = panic::catch_unwind(AssertUnwindSafe(|| job(&mut connections)));
                        }
                    })
                    .ok()
                    .map(|_| tx)
            })
            .as_ref()
    }

    /// Run `f` on the WMI worker thread with its cached connections.
    ///
    /// Returns `None` when the worker is unavailable or does not answer
    /// within [`REPLY_TIMEOUT`]. Jobs run one at a time, so `f` must not call
    /// `with_wmi` itself, and it returns owned data because it cannot borrow
    /// from the caller.
    pub fn with_wmi<T: Send + 'static>(
        f: impl FnOnce(&mut WmiConnections) -> T + Send + 'static,
    ) -> Option<T> {
        with_wmi_timeout(REPLY_TIMEOUT, f)
    }

    pub(super) fn with_wmi_timeout<T: Send + 'static>(
        timeout: Duration,
        f: impl FnOnce(&mut WmiConnections) -> T + Send + 'static,
    ) -> Option<T> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |connections| {
            // The caller may have timed out and dropped the receiver.
            let _ = reply_tx.send(f(connections));
        });
        worker()?.send(job).ok()?;
        reply_rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A cache whose factory counts its calls per namespace and fails for
    /// namespaces starting with `missing`.
    fn counting_cache() -> (ConnectionCache<String>, Rc<Cell<u32>>) {
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let cache = ConnectionCache::with_factory(move |namespace: &str| {
            counter.set(counter.get() + 1);
            (!namespace.starts_with("missing")).then(|| format!("conn:{namespace}"))
        });
        (cache, calls)
    }

    #[test]
    fn connection_is_created_once_and_reused() {
        let (mut cache, calls) = counting_cache();
        let first = cache.get("root\\Intel").map(std::ptr::from_ref);
        for _ in 0..9 {
            assert_eq!(cache.get("root\\Intel").map(std::ptr::from_ref), first);
        }
        assert_eq!(calls.get(), 1);
        assert_eq!(cache.get("root\\Intel").unwrap(), "conn:root\\Intel");
    }

    #[test]
    fn namespaces_are_cached_independently() {
        let (mut cache, calls) = counting_cache();
        assert!(cache.get("root\\WMI").is_some());
        assert!(cache.get("ROOT\\CIMV2").is_some());
        assert!(cache.get("root\\WMI").is_some());
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn invalidate_forces_a_reconnect() {
        let (mut cache, calls) = counting_cache();
        assert!(cache.get("root\\Intel").is_some());
        cache.invalidate("root\\Intel");
        assert!(cache.get("root\\Intel").is_some());
        assert_eq!(calls.get(), 2);
        cache.invalidate("root\\NeverOpened");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn failed_connection_is_retried_on_next_use() {
        let (mut cache, calls) = counting_cache();
        assert!(cache.get("missing\\ns").is_none());
        assert!(cache.get("missing\\ns").is_none());
        assert_eq!(calls.get(), 2, "a failure is not cached");
    }

    /// The worker itself: one thread, one connection per namespace, shared
    /// by every caller.
    #[cfg(target_os = "windows")]
    #[test]
    fn calls_from_different_threads_share_one_connection() {
        let results: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    with_wmi(|connections| {
                        let address = connections
                            .get(CIMV2)
                            .map(|c| std::ptr::from_ref(c) as usize);
                        (address, std::thread::current().id())
                    })
                })
            })
            .map(|handle| handle.join().unwrap().expect("worker answered"))
            .collect();
        assert!(results[0].0.is_some(), "cimv2 connection opened");
        assert!(results.iter().all(|r| *r == results[0]), "{results:?}");
        assert_ne!(results[0].1, std::thread::current().id());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_hung_job_returns_none_after_the_timeout() {
        use std::time::{Duration, Instant};
        let started = Instant::now();
        let answer = worker::with_wmi_timeout(Duration::from_millis(200), |_| {
            std::thread::sleep(Duration::from_millis(1500));
            1
        });
        assert_eq!(answer, None);
        let waited = started.elapsed();
        assert!(waited < Duration::from_millis(1200), "waited {waited:?}");
    }
}
