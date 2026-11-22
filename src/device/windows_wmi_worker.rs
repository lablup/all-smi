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
//! A caller that stops waiting marks its job cancelled, and the worker skips
//! a cancelled job instead of running it late: while a provider is wedged
//! the queue would otherwise grow by one job per tick, and once it cleared
//! the worker would run a backlog of queries nobody reads.
//!
//! [`ConnectionCache`], [`CancellableJob`], [`run_jobs`] and [`submit`] are
//! generic over the connection type, so caching, invalidation and the
//! cancel-on-timeout path are unit-tested on every runner with fakes.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

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

/// A queued closure plus the flag its caller sets once it stops waiting.
pub struct CancellableJob<C> {
    cancelled: Arc<AtomicBool>,
    run: Box<dyn FnOnce(&mut C) + Send>,
}

impl<C> CancellableJob<C> {
    /// Wrap `run`; the returned flag cancels the job while it is queued.
    pub fn new(run: impl FnOnce(&mut C) + Send + 'static) -> (Self, Arc<AtomicBool>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = Self {
            cancelled: Arc::clone(&cancelled),
            run: Box::new(run),
        };
        (job, cancelled)
    }

    /// Run the job against `target` unless its caller already gave up.
    /// Returns whether it ran.
    pub fn run_unless_cancelled(self, target: &mut C) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        (self.run)(target);
        true
    }
}

/// The worker loop: run each job in arrival order until every sender is
/// gone, skipping cancelled ones.
pub fn run_jobs<C>(jobs: Receiver<CancellableJob<C>>, target: &mut C) {
    for job in jobs {
        // A panicking job must not take the worker, and with it every later
        // reading, down too.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| job.run_unless_cancelled(target)));
    }
}

/// Queue `f` on the worker behind `jobs` and wait up to `timeout` for its
/// answer. On timeout the job is cancelled, so a worker that is stuck on an
/// earlier job drops it instead of running it after nobody is waiting.
pub fn submit<C: 'static, T: Send + 'static>(
    jobs: &Sender<CancellableJob<C>>,
    timeout: Duration,
    f: impl FnOnce(&mut C) -> T + Send + 'static,
) -> Option<T> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let (job, cancelled) = CancellableJob::new(move |target: &mut C| {
        // The caller may have timed out and dropped the receiver.
        let _ = reply_tx.send(f(target));
    });
    jobs.send(job).ok()?;
    match reply_rx.recv_timeout(timeout) {
        Ok(answer) => Some(answer),
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            None
        }
    }
}

#[cfg(target_os = "windows")]
pub use worker::*;

#[cfg(target_os = "windows")]
mod worker {
    use super::{CancellableJob, ConnectionCache, run_jobs, submit};
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

    type Job = CancellableJob<WmiConnections>;

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
                        run_jobs(rx, &mut connections);
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
        submit(worker()?, timeout, f)
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

    /// Issue #501 review: a job whose caller timed out must not run later.
    /// The first job wedges the worker past three callers' timeouts; once it
    /// clears, only the job whose caller is still waiting runs.
    #[test]
    fn timed_out_jobs_are_skipped_when_the_worker_clears() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Instant;

        let (jobs, rx) = mpsc::channel::<CancellableJob<Vec<u32>>>();
        let worker = std::thread::spawn(move || {
            let mut ran = Vec::new();
            run_jobs(rx, &mut ran);
            ran
        });

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (wedged_tx, wedged_rx) = mpsc::channel::<()>();
        let (job, _) = CancellableJob::new(move |ran: &mut Vec<u32>| {
            let _ = wedged_tx.send(());
            // Bounded: the test releases it, and a lost release ends it too.
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            ran.push(0);
        });
        jobs.send(job).unwrap();
        wedged_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker started the wedged job");

        let late_runs = Arc::new(AtomicUsize::new(0));
        for tick in 1..=3 {
            let late_runs = Arc::clone(&late_runs);
            let started = Instant::now();
            let answer = submit(
                &jobs,
                Duration::from_millis(50),
                move |ran: &mut Vec<u32>| {
                    late_runs.fetch_add(1, Ordering::Relaxed);
                    ran.push(tick);
                    tick
                },
            );
            assert_eq!(answer, None, "tick {tick} must time out");
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        release_tx.send(()).unwrap();
        let answer = submit(&jobs, Duration::from_secs(5), |ran: &mut Vec<u32>| {
            ran.push(99);
            99
        });
        assert_eq!(answer, Some(99));
        drop(jobs);
        let ran = worker.join().unwrap();
        assert_eq!(ran, [0, 99], "timed-out jobs ran late");
        assert_eq!(late_runs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_panicking_job_does_not_stop_the_loop() {
        let (jobs, rx) = mpsc::channel::<CancellableJob<u32>>();
        let (bad, _) = CancellableJob::new(|_: &mut u32| panic!("job panicked"));
        let (good, _) = CancellableJob::new(|n: &mut u32| *n += 1);
        let (cancelled, flag) = CancellableJob::new(|n: &mut u32| *n += 100);
        flag.store(true, Ordering::Release);
        jobs.send(bad).unwrap();
        jobs.send(cancelled).unwrap();
        jobs.send(good).unwrap();
        drop(jobs);
        let mut count = 0;
        run_jobs(rx, &mut count);
        assert_eq!(count, 1);
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
