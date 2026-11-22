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

//! `record --source=remote --hostfile` scrapes the hostfile's hosts
//! (issue #500: it used to pass only `--hosts` to every tick and record a
//! header with no frames).

use std::sync::Arc;
use std::time::Duration;

use axum::{Router, routing::get};

use super::writer::Codec;
use super::{RecorderOptions, StopSignal, run_remote};
use crate::cli::{RecordSource, SnapshotIncludes};

const METRICS: &str = r#"
all_smi_gpu_info{gpu="Recorded GPU",instance="rec-node",gpu_uuid="GPU-REC",gpu_index="0",type="GPU"} 1
all_smi_gpu_memory_total_bytes{gpu="Recorded GPU",instance="rec-node",gpu_uuid="GPU-REC",gpu_index="0"} 17179869184
all_smi_gpu_utilization{gpu="Recorded GPU",instance="rec-node",gpu_uuid="GPU-REC",gpu_index="0"} 42
"#;

#[tokio::test]
async fn hostfile_only_recording_scrapes_the_hostfile_hosts() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route("/metrics", get(|| async { METRICS }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let dir = tempfile::tempdir().unwrap();
    let hostfile = dir.path().join("hosts.txt");
    let host = format!("http://{addr}");
    std::fs::write(&hostfile, format!("# recorder targets\n{host}\n")).unwrap();
    let output = dir.path().join("rec.ndjson");

    let opts = RecorderOptions {
        output: output.clone(),
        interval: Duration::from_millis(300),
        duration: Some(Duration::from_millis(700)),
        source: RecordSource::Remote,
        hosts: Vec::new(),
        hostfile: Some(hostfile.to_str().unwrap().to_string()),
        includes: SnapshotIncludes {
            gpu: true,
            cpu: false,
            memory: false,
            chassis: false,
            process: false,
            storage: false,
        },
        max_size: 64 * 1024 * 1024,
        max_files: 1,
        codec: Codec::Plain,
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        run_remote(opts, Arc::new(StopSignal::default())),
    )
    .await
    .expect("recording finished")
    .expect("recording succeeded");

    let text = std::fs::read_to_string(&output).unwrap();
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["header"], true);
    assert_eq!(header["hosts"], serde_json::json!([host]));
    let frames: Vec<&str> = lines.filter(|l| l.contains("Recorded GPU")).collect();
    assert!(
        !frames.is_empty(),
        "expected data frames from the hostfile host:\n{text}"
    );
}
