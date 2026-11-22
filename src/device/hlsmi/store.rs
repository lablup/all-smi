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

use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::parser::{GaudiMetricsData, parse_hlsmi_output};

/// The most recent complete hl-smi CSV section and its sequence number.
///
/// Only the newest snapshot is ever read, so the store keeps a single slot
/// instead of a history buffer. `seq` grows by one per push, which lets
/// readers tell whether the slot changed since they last parsed it.
#[derive(Debug, Default)]
struct LatestSnapshot {
    seq: u64,
    section: Option<String>,
}

/// The parse of the snapshot whose sequence number is `seq`.
#[derive(Debug, Default)]
struct ParsedSnapshot {
    seq: u64,
    data: Option<GaudiMetricsData>,
}

/// Latest-slot store for hl-smi output.
///
/// The reader thread pushes complete sections; `get_latest_data` parses a
/// section only once, however many collection ticks read it (issue #501).
#[derive(Debug, Default)]
pub struct MetricsStore {
    latest: Mutex<LatestSnapshot>,
    parsed: Mutex<ParsedSnapshot>,
    /// Number of `parse_hlsmi_output` calls, for the reparse tests.
    #[cfg(test)]
    parse_count: AtomicUsize,
}

impl MetricsStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the latest snapshot with a newly completed section.
    pub fn push_section(&self, section: String) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        latest.seq = latest.seq.wrapping_add(1);
        latest.section = Some(section);
    }

    /// Parse of the latest snapshot.
    ///
    /// A snapshot that was already parsed returns the cached result without
    /// cloning or reparsing it. When the newest section does not parse, the
    /// last good result is returned instead, as before.
    pub fn get_latest_data(&self) -> Result<GaudiMetricsData, Box<dyn std::error::Error>> {
        // Lock order: `parsed` then `latest`. The reader thread only takes
        // `latest`, so this cannot deadlock.
        let mut parsed = self.parsed.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = {
            let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            (latest.seq != parsed.seq).then(|| (latest.seq, latest.section.clone()))
        };

        if let Some((seq, section)) = fresh {
            parsed.seq = seq;
            match section {
                Some(section) => {
                    #[cfg(test)]
                    self.parse_count.fetch_add(1, Ordering::Relaxed);
                    if let Ok(data) = parse_hlsmi_output(&section) {
                        parsed.data = Some(data);
                    }
                }
                // `clear` emptied the slot: forget the old parse too.
                None => parsed.data = None,
            }
        }

        parsed
            .data
            .clone()
            .ok_or_else(|| "No hl-smi data available".into())
    }

    /// Clear all stored data
    pub fn clear(&self) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        latest.seq = latest.seq.wrapping_add(1);
        latest.section = None;
    }

    #[cfg(test)]
    fn parse_count(&self) -> usize {
        self.parse_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const TWO_DEVICES: &str = "0, 01P4-HL3090A0-18-U4V193-22-07-00, HL-325L, 1.22.1-97ec1a4, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %\n\
                               1, 01P4-HL3090A0-18-U4V298-03-04-04, HL-325L, 1.22.1-97ec1a4, 131072 MiB, 672 MiB, 130400 MiB, 230 W, 850 W, 39 C, 0 %";

    fn section(uuid: &str) -> String {
        format!(
            "0, {uuid}, HL-325L, 1.22.1, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %"
        )
    }

    #[test]
    fn unchanged_snapshot_is_parsed_once() {
        let store = MetricsStore::new();
        store.push_section(section("UUID-A"));
        for _ in 0..3 {
            let data = store.get_latest_data().unwrap();
            assert_eq!(data.devices[0].uuid, "UUID-A");
        }
        assert_eq!(store.parse_count(), 1);

        store.push_section(section("UUID-B"));
        assert_eq!(store.get_latest_data().unwrap().devices[0].uuid, "UUID-B");
        assert_eq!(store.get_latest_data().unwrap().devices[0].uuid, "UUID-B");
        assert_eq!(store.parse_count(), 2);
    }

    #[test]
    fn latest_push_wins() {
        let store = MetricsStore::new();
        for i in 0..10 {
            store.push_section(section(&format!("UUID-{i}")));
        }
        assert_eq!(store.get_latest_data().unwrap().devices[0].uuid, "UUID-9");
        assert_eq!(
            store.parse_count(),
            1,
            "intermediate pushes are never parsed"
        );
    }

    #[test]
    fn unparsable_section_keeps_the_last_good_data() {
        let store = MetricsStore::new();
        store.push_section(section("UUID-A"));
        assert!(store.get_latest_data().is_ok());
        store.push_section(section("UUID-B").replacen('0', "x", 1));
        assert_eq!(store.get_latest_data().unwrap().devices[0].uuid, "UUID-A");
        assert_eq!(store.get_latest_data().unwrap().devices[0].uuid, "UUID-A");
        assert_eq!(store.parse_count(), 2, "the bad section is parsed once");
    }

    #[test]
    fn empty_and_cleared_store_report_no_data() {
        let store = MetricsStore::new();
        assert!(store.get_latest_data().is_err());
        store.push_section(section("UUID-A"));
        assert!(store.get_latest_data().is_ok());
        store.clear();
        assert!(store.get_latest_data().is_err());
    }

    #[test]
    fn concurrent_pushes_and_reads() {
        let store = Arc::new(MetricsStore::new());
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        store.push_section(section(&format!("UUID-{i}-{j}")));
                        let _ = store.get_latest_data();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.latest.lock().unwrap().seq, 100);
        assert!(store.get_latest_data().is_ok());
    }

    #[test]
    fn test_get_latest_data() {
        let store = MetricsStore::new();
        store.push_section(TWO_DEVICES.to_string());

        let data = store.get_latest_data().unwrap();
        assert_eq!(data.devices.len(), 2);
        assert_eq!(data.devices[0].index, 0);
        assert_eq!(data.devices[0].driver_version, "1.22.1");
        assert_eq!(data.devices[1].index, 1);
    }
}
