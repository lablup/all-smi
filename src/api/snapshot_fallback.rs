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

//! State behind the `/snapshot` fresh-collect fallback (issue #503).
//!
//! `/snapshot` normally serves the frame the collection loop published.
//! When that frame is missing or stale and no new one arrives within an
//! interval, the handler collects for itself. That path used to build a
//! complete reader set per request, on an async worker (NVML init, CLI
//! probes, a synchronous mount-table listing), and discard the result, so
//! every stale request repeated all of it.
//!
//! This holds what makes the fallback cheap to repeat:
//!
//! * the collector, built once per process on the blocking pool,
//! * a [`SectionGate`], so a hung reader holds one blocking thread however
//!   many requests arrive,
//! * the last result, reused while it is younger than the collection
//!   interval and covers the sections a request asks for.
//!
//! A fallback snapshot is never published on the bus: it may hold only the
//! sections one client asked for, and it has no sequence number.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::OnceCell;
use tokio::time::Instant;

use crate::api::handlers::snapshot::SectionFilter;
use crate::cli::SnapshotIncludes;
use crate::snapshot::{
    DefaultSnapshotCollector, SectionGate, Snapshot, SnapshotCollector, collect_once_gated,
};

/// Builds the fallback collector. Runs on the blocking pool.
pub(crate) type CollectorFactory = Arc<dyn Fn() -> Arc<dyn SnapshotCollector> + Send + Sync>;

/// The last fallback collection.
struct FallbackResult {
    collected_at: Instant,
    filter: SectionFilter,
    snapshot: Arc<Snapshot>,
}

pub(crate) struct SnapshotFallback {
    factory: CollectorFactory,
    collector: OnceCell<Arc<dyn SnapshotCollector>>,
    gate: SectionGate,
    last: Mutex<Option<FallbackResult>>,
    /// Single-flight lock: at most one fallback collection is in flight.
    lock: tokio::sync::Mutex<()>,
}

impl Default for SnapshotFallback {
    fn default() -> Self {
        Self::new(Arc::new(|| {
            Arc::new(DefaultSnapshotCollector::new()) as Arc<dyn SnapshotCollector>
        }))
    }
}

impl SnapshotFallback {
    pub(crate) fn new(factory: CollectorFactory) -> Self {
        Self {
            factory,
            collector: OnceCell::new(),
            gate: SectionGate::new(),
            last: Mutex::new(None),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Acquire the single-flight lock.
    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// The last fallback result, if it is younger than `max_age` and was
    /// collected with every section `filter` asks for.
    pub(crate) fn cached(
        &self,
        filter: &SectionFilter,
        max_age: Duration,
    ) -> Option<Arc<Snapshot>> {
        let last = match self.last.lock() {
            Ok(last) => last,
            Err(poisoned) => poisoned.into_inner(),
        };
        last.as_ref()
            .filter(|result| {
                result.collected_at.elapsed() < max_age && result.filter.covers(filter)
            })
            .map(|result| result.snapshot.clone())
    }

    /// The collector, built on first use on the blocking pool and kept for
    /// the life of the process.
    async fn collector(&self) -> Result<Arc<dyn SnapshotCollector>, String> {
        self.collector
            .get_or_try_init(|| async {
                let factory = self.factory.clone();
                tokio::task::spawn_blocking(move || factory())
                    .await
                    .map_err(|e| format!("building the snapshot collector failed: {e}"))
            })
            .await
            .cloned()
    }

    /// Collect the sections `filter` asks for and remember the result.
    /// The caller holds [`Self::lock`].
    pub(crate) async fn collect(
        &self,
        filter: &SectionFilter,
        per_reader_timeout: Duration,
    ) -> Result<Arc<Snapshot>, String> {
        let collector = self.collector().await?;
        // Map the caller-visible `SectionFilter` onto the collector-level
        // `SnapshotIncludes` so the response carries exactly the sections
        // the client asked for, as the cached path does by filtering the
        // always-complete background frame.
        let includes = SnapshotIncludes {
            gpu: filter.gpu,
            cpu: filter.cpu,
            memory: filter.memory,
            chassis: filter.chassis,
            process: filter.process,
            storage: filter.storage,
        };
        let snapshot = Arc::new(
            collect_once_gated(collector, &includes, per_reader_timeout, &self.gate).await,
        );
        let mut last = match self.last.lock() {
            Ok(last) => last,
            Err(poisoned) => poisoned.into_inner(),
        };
        *last = Some(FallbackResult {
            collected_at: Instant::now(),
            filter: *filter,
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }
}
