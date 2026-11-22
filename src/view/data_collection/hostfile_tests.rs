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

use std::time::{Duration, SystemTime};

use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

/// Rewrite the file and push its mtime forward so the change is visible
/// even on filesystems with coarse timestamps.
fn rewrite(path: &Path, content: &str, bump_secs: u64) {
    std::fs::write(path, content).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(bump_secs))
        .unwrap();
}

#[test]
fn parse_skips_comments_blanks_and_invalid_entries() {
    let parsed = parse_hostfile_content("# head\n\n  http://a:9090  \nnot a url\nb:9091\n");
    assert_eq!(parsed.hosts, strings(&["http://a:9090", "b:9091"]));
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
}

#[test]
fn parse_caps_host_count_with_one_warning() {
    let content: String = (0..MAX_HOSTFILE_HOSTS + 5)
        .map(|i| format!("node-{i}:9090\n"))
        .collect();
    let parsed = parse_hostfile_content(&content);
    assert_eq!(parsed.hosts.len(), MAX_HOSTFILE_HOSTS);
    assert_eq!(parsed.warnings.len(), 1);
}

#[test]
fn merge_hosts_dedups_by_identity_preserving_order() {
    let merged = merge_hosts(
        &strings(&["http://a:9090", "b:9090"]),
        &strings(&["a:9090", "c:9090", "http://b:9090"]),
    );
    assert_eq!(merged, strings(&["http://a:9090", "b:9090", "c:9090"]));
}

#[test]
fn resolve_hosts_merges_cli_and_file_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, "a:9090\nc:9090\n").unwrap();
    let resolved = resolve_hosts(&strings(&["a:9090", "b:9090"]), path.to_str()).unwrap();
    assert_eq!(resolved.hosts, strings(&["a:9090", "b:9090", "c:9090"]));

    let none = resolve_hosts(&strings(&["a:9090"]), None).unwrap();
    assert_eq!(none.hosts, strings(&["a:9090"]));
}

#[tokio::test]
async fn unchanged_hostfile_is_read_once_and_changes_are_picked_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, "a:9090\n").unwrap();

    let (mut watcher, warnings) = HostfileWatcher::open(path.to_str().unwrap()).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(watcher.hosts(), strings(&["a:9090"]).as_slice());

    for _ in 0..3 {
        assert_eq!(watcher.refresh().await, strings(&["a:9090"]).as_slice());
    }
    assert_eq!(
        watcher.read_count(),
        1,
        "unchanged file must not be re-read"
    );

    rewrite(&path, "a:9090\nb:9090\n", 5);
    assert_eq!(
        watcher.refresh().await,
        strings(&["a:9090", "b:9090"]).as_slice()
    );
    assert_eq!(watcher.read_count(), 2);
    watcher.refresh().await;
    assert_eq!(watcher.read_count(), 2);
}

#[tokio::test]
async fn transient_failure_keeps_previous_host_list() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, "a:9090\n").unwrap();
    let (mut watcher, _) = HostfileWatcher::open(path.to_str().unwrap()).unwrap();

    std::fs::remove_file(&path).unwrap();
    assert_eq!(watcher.refresh().await, strings(&["a:9090"]).as_slice());
    assert_eq!(watcher.refresh().await, strings(&["a:9090"]).as_slice());

    rewrite(&path, "b:9090\n", 10);
    assert_eq!(watcher.refresh().await, strings(&["b:9090"]).as_slice());
}

#[tokio::test]
async fn tilde_prefixed_hostfile_reloads() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("hosts");
    std::fs::write(&path, "a:9090\n").unwrap();

    let (mut watcher, initial) = {
        let _guard = crate::common::test_env::lock_env();
        let previous = std::env::var_os("HOME");
        // SAFETY: the shared env lock serializes every test that mutates the
        // process environment, and HOME is restored before the lock drops.
        unsafe { std::env::set_var("HOME", home.path()) };
        let opened = HostfileWatcher::open("~/hosts");
        let read = read_hostfile(Path::new("~/hosts"));
        // SAFETY: as above.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let (watcher, _) = opened.expect("~/hosts must resolve against HOME");
        (watcher, read.expect("read_hostfile expands ~/").hosts)
    };
    assert_eq!(initial, strings(&["a:9090"]));

    rewrite(&path, "a:9090\nb:9090\n", 5);
    assert_eq!(
        watcher.refresh().await,
        strings(&["a:9090", "b:9090"]).as_slice(),
        "reload must use the expanded path, not the raw `~/hosts`"
    );
}

#[test]
fn bounded_read_accepts_exactly_the_cap_and_rejects_one_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, "a".repeat(16)).unwrap();
    assert_eq!(read_bounded(&path, 16).unwrap().len(), 16);
    let err = read_bounded(&path, 15).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("too large"), "{err}");
}

#[tokio::test]
async fn bounded_async_read_accepts_exactly_the_cap_and_rejects_one_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, "a".repeat(16)).unwrap();
    assert_eq!(read_bounded_async(&path, 16).await.unwrap().len(), 16);
    let err = read_bounded_async(&path, 15).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn bounded_read_rejects_invalid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts");
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert_eq!(
        read_bounded(&path, 16).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
