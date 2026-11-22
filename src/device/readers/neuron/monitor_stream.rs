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

//! The long-lived `neuron-monitor` stream.
//!
//! `neuron-monitor` has no one-shot mode: it prints one NDJSON record at
//! start and another every 5 s. Spawning it for each collection tick, taking
//! the first record and killing it cost a process and a helper thread per
//! tick, and only ever saw the t=0 record. Instead one child runs for as long
//! as it keeps producing records, a single reader thread keeps the latest
//! parsed record, and a collection tick reads that without blocking.
//!
//! # Lifetime of the child
//!
//! The reader thread spawns the child, reads it, and reaps it. The child is
//! spawned with `PR_SET_PDEATHSIG`, which the kernel delivers when the
//! *thread* that created the child exits, not the process. Collection runs on
//! short-lived blocking-pool threads, so spawning from one of those would get
//! the monitor killed whenever that pool thread retired. Spawning from the
//! reader thread ties the signal to the one thread that lives exactly as long
//! as the child is wanted, and to the process as a whole when all-smi exits.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{
    MONITOR_FIRST_RECORD_TIMEOUT, MONITOR_RECORD_CAP_BYTES, MONITOR_RETRY_INTERVAL,
    MonitorSnapshot, parse_monitor_record,
};
use crate::device::readers::common_cache::RetryGate;
use crate::utils::command::new_command;

/// A record older than this is reported as absent. Three of the tool's 5 s
/// periods: one late record is tolerated, a stalled stream is not shown as
/// current utilization.
pub(super) const MONITOR_STALE_AFTER: Duration = Duration::from_secs(15);

struct StreamState {
    /// The newest parsed record and when it arrived.
    latest: Option<(Instant, Arc<MonitorSnapshot>)>,
    /// A reader thread is alive (spawning, reading, or reaping).
    running: bool,
    /// When the child was started or last produced a record. A running
    /// stream silent for longer than [`MONITOR_STALE_AFTER`] is killed.
    last_activity: Option<Instant>,
    /// The one-time wait for the very first record has been spent.
    first_wait_done: bool,
    /// The running child, held here so a wedged stream can be killed from a
    /// collection tick. The reader thread takes it back to reap it.
    child: Option<Child>,
}

/// State shared between collection ticks and the reader thread.
pub(super) struct MonitorShared {
    state: Mutex<StreamState>,
    /// Signalled when a record arrives or the stream ends.
    changed: Condvar,
    /// Suppresses respawning for [`MONITOR_RETRY_INTERVAL`] after the stream
    /// ended: the daemon and runtime may become available later, but a tool
    /// that keeps dying must not be respawned every tick.
    backoff: RetryGate,
}

impl MonitorShared {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(StreamState {
                latest: None,
                running: false,
                last_activity: None,
                first_wait_done: false,
                child: None,
            }),
            changed: Condvar::new(),
            backoff: RetryGate::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, StreamState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keep `snapshot` as the latest record. A record is proof the tool
    /// works, so it also clears the respawn back-off.
    fn store(&self, snapshot: MonitorSnapshot, now: Instant) {
        let mut state = self.lock();
        state.latest = Some((now, Arc::new(snapshot)));
        state.last_activity = Some(now);
        drop(state);
        self.backoff.clear();
        self.changed.notify_all();
    }

    /// The stream is over (EOF, read error, oversized record, or the child
    /// could not be spawned): mark it stopped and start the back-off.
    fn stream_ended(&self, now: Instant) {
        self.backoff.fail(now, MONITOR_RETRY_INTERVAL);
        self.lock().running = false;
        self.changed.notify_all();
    }

    /// Whether a respawn is currently suppressed.
    pub(super) fn backoff_active(&self, now: Instant) -> bool {
        self.backoff.is_blocked(now)
    }

    /// The latest record, unless it is older than [`MONITOR_STALE_AFTER`].
    pub(super) fn fresh_snapshot(&self, now: Instant) -> Option<Arc<MonitorSnapshot>> {
        fresh(self.lock().latest.as_ref(), now)
    }
}

fn fresh(
    latest: Option<&(Instant, Arc<MonitorSnapshot>)>,
    now: Instant,
) -> Option<Arc<MonitorSnapshot>> {
    latest
        .filter(|(received, _)| now.saturating_duration_since(*received) <= MONITOR_STALE_AFTER)
        .map(|(_, snapshot)| Arc::clone(snapshot))
}

/// Read one NDJSON line from the monitor and keep it if it parses.
///
/// Returns `false` when the stream is over: EOF, a read error, or a record
/// that reached [`MONITOR_RECORD_CAP_BYTES`] without a newline. A line that
/// does not parse is skipped; the stream goes on.
pub(super) fn pump_monitor_line<R: BufRead>(reader: &mut R, shared: &MonitorShared) -> bool {
    let mut line = String::new();
    match reader
        .by_ref()
        .take(MONITOR_RECORD_CAP_BYTES)
        .read_line(&mut line)
    {
        Ok(0) | Err(_) => return false,
        Ok(_) => {}
    }
    if !line.ends_with('\n') && line.len() as u64 >= MONITOR_RECORD_CAP_BYTES {
        return false;
    }
    if let Some(snapshot) = parse_monitor_record(&line) {
        shared.store(snapshot, Instant::now());
    }
    true
}

/// Consume the monitor's stdout until it ends, run `reap`, then record the
/// failure and mark the stream stopped.
///
/// `stdout` is `None` when the child could not be spawned. `reap` runs
/// before the stream is marked stopped, so a respawn never overlaps the
/// previous child.
pub(super) fn read_monitor_stream<R: Read>(
    stdout: Option<R>,
    shared: &MonitorShared,
    reap: impl FnOnce(),
) {
    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout);
        while pump_monitor_line(&mut reader, shared) {}
    }
    reap();
    shared.stream_ended(Instant::now());
}

/// Spawn `neuron-monitor` so that it dies with the calling thread.
fn spawn_monitor(bin: &Path, args: &[OsString]) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;

    // Read before the fork: the child compares its parent against this to
    // catch the parent exiting before `PR_SET_PDEATHSIG` took effect.
    let parent = std::process::id() as libc::pid_t;
    let mut command = new_command(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before `exec`. It only
    // makes the `prctl` and `getppid` syscalls, both async-signal-safe, and
    // builds an `io::Error` from a raw errno, which does not allocate. It
    // touches no memory shared with the parent. A `prctl` failure is
    // ignored: the monitor then merely lacks the kill-on-parent-death
    // guarantee. If the parent already exited between `fork` and `prctl`,
    // the signal will never come, so the child refuses to `exec`.
    unsafe {
        command.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Body of the reader thread: spawn the child, read it to the end, reap it.
fn run_monitor(bin: PathBuf, args: Vec<OsString>, shared: Arc<MonitorShared>) {
    let stdout = match spawn_monitor(&bin, &args) {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            shared.lock().child = Some(child);
            stdout
        }
        Err(_) => None,
    };
    read_monitor_stream(stdout, &shared, || {
        let child = shared.lock().child.take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    });
}

/// Owner of the long-lived `neuron-monitor` child.
pub(super) struct MonitorStream {
    bin: PathBuf,
    /// Arguments for `bin`. Empty in production; tests run a fake monitor
    /// script through `/bin/sh` instead of executing the file they just
    /// wrote, which another test thread's concurrent fork can make fail
    /// with `ETXTBSY`.
    args: Vec<OsString>,
    shared: Arc<MonitorShared>,
}

impl MonitorStream {
    pub(super) fn new(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            args: Vec::new(),
            shared: Arc::new(MonitorShared::new()),
        }
    }

    /// A stream whose child is `bin` run with `args`.
    #[cfg(test)]
    fn with_args(bin: impl Into<PathBuf>, args: Vec<OsString>) -> Self {
        let mut stream = Self::new(bin);
        stream.args = args;
        stream
    }

    /// The latest monitor record, without blocking.
    ///
    /// Starts the child when none is running, the binary exists and the
    /// back-off allows it. The first call ever waits up to
    /// [`MONITOR_FIRST_RECORD_TIMEOUT`] for the first record so a one-shot
    /// command still gets utilization; no later call waits. A running stream
    /// that has been silent for [`MONITOR_STALE_AFTER`] is killed, which
    /// ends the reader thread and lets a later call respawn it.
    pub(super) fn latest(&self, now: Instant) -> Option<Arc<MonitorSnapshot>> {
        let mut state = self.shared.lock();
        if state.running {
            let silent = state
                .last_activity
                .is_some_and(|at| now.saturating_duration_since(at) > MONITOR_STALE_AFTER);
            if silent && let Some(child) = state.child.as_mut() {
                let _ = child.kill();
            }
        } else if self.bin.exists() && !self.shared.backoff_active(now) {
            state.running = true;
            state.last_activity = Some(now);
            let bin = self.bin.clone();
            let args = self.args.clone();
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("neuron-monitor-reader".to_string())
                .spawn(move || run_monitor(bin, args, shared));
            if spawned.is_err() {
                state.running = false;
                self.shared.backoff.fail(now, MONITOR_RETRY_INTERVAL);
            } else if !state.first_wait_done {
                state.first_wait_done = true;
                state = self
                    .shared
                    .changed
                    .wait_timeout_while(state, MONITOR_FIRST_RECORD_TIMEOUT, |state| {
                        state.running && state.latest.is_none()
                    })
                    .map(|(state, _)| state)
                    .unwrap_or_else(|e| e.into_inner().0);
            }
        }
        drop(state);
        self.shared.fresh_snapshot(now)
    }
}

impl Drop for MonitorStream {
    fn drop(&mut self) {
        // Killing the child closes its stdout; the reader thread then sees
        // EOF, reaps it and exits.
        if let Some(child) = self.shared.lock().child.as_mut() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{MONITOR_NO_RUNTIME, MONITOR_WITH_RUNTIME};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn each_line_replaces_the_latest_snapshot_and_eof_ends_the_stream() {
        let shared = MonitorShared::new();
        let stream = format!("{MONITOR_NO_RUNTIME}\n{MONITOR_WITH_RUNTIME}\n");
        let mut reader = Cursor::new(stream);
        assert!(shared.fresh_snapshot(Instant::now()).is_none());

        assert!(pump_monitor_line(&mut reader, &shared));
        let first = shared.fresh_snapshot(Instant::now()).expect("first record");
        assert!(first.utilization.is_empty());
        assert!(first.hardware.is_some());

        assert!(pump_monitor_line(&mut reader, &shared));
        let second = shared
            .fresh_snapshot(Instant::now())
            .expect("second record");
        assert_eq!(second.utilization.get(&0).copied(), Some(42.5));
        assert_eq!(second.utilization.get(&1).copied(), Some(7.25));

        // EOF: nothing more to read.
        assert!(!pump_monitor_line(&mut reader, &shared));
    }

    #[test]
    fn eof_records_a_failure_that_expires_after_the_retry_interval() {
        let shared = MonitorShared::new();
        shared.lock().running = true;
        let before = Instant::now();
        assert!(!shared.backoff_active(before));

        let reaped = std::cell::Cell::new(false);
        read_monitor_stream(
            Some(Cursor::new(format!("{MONITOR_WITH_RUNTIME}\n"))),
            &shared,
            || reaped.set(true),
        );

        assert!(reaped.get());
        assert!(!shared.lock().running);
        let after = Instant::now();
        assert!(shared.backoff_active(after));
        assert!(!shared.backoff_active(after + MONITOR_RETRY_INTERVAL));
        // The record read before EOF is still served until it goes stale.
        assert!(shared.fresh_snapshot(after).is_some());
    }

    #[test]
    fn a_child_that_could_not_be_spawned_records_a_failure() {
        let shared = MonitorShared::new();
        shared.lock().running = true;
        read_monitor_stream(None::<Cursor<Vec<u8>>>, &shared, || {});
        assert!(!shared.lock().running);
        assert!(shared.backoff_active(Instant::now()));
    }

    #[test]
    fn a_stale_snapshot_is_reported_absent() {
        let shared = MonitorShared::new();
        let mut reader = Cursor::new(format!("{MONITOR_WITH_RUNTIME}\n"));
        // The record arrives between `before` and `after`.
        let before = Instant::now();
        assert!(pump_monitor_line(&mut reader, &shared));
        let after = Instant::now();

        assert!(
            shared
                .fresh_snapshot(before + MONITOR_STALE_AFTER)
                .is_some()
        );
        assert!(
            shared
                .fresh_snapshot(after + MONITOR_STALE_AFTER + Duration::from_millis(1))
                .is_none()
        );
    }

    #[test]
    fn an_unparseable_line_is_skipped_without_ending_the_stream() {
        let shared = MonitorShared::new();
        let mut reader = Cursor::new(format!("not json\n\n{MONITOR_WITH_RUNTIME}\n"));

        assert!(pump_monitor_line(&mut reader, &shared));
        assert!(pump_monitor_line(&mut reader, &shared));
        assert!(shared.fresh_snapshot(Instant::now()).is_none());
        assert!(pump_monitor_line(&mut reader, &shared));
        assert!(shared.fresh_snapshot(Instant::now()).is_some());
    }

    #[test]
    fn a_record_over_the_size_cap_ends_the_stream() {
        let shared = MonitorShared::new();
        let oversized = "x".repeat(MONITOR_RECORD_CAP_BYTES as usize + 16);
        let mut reader = Cursor::new(oversized);
        assert!(!pump_monitor_line(&mut reader, &shared));
    }

    /// A stream over a stand-in for `neuron-monitor` that appends a line to
    /// `runs` each time it starts, prints one record, and then runs `tail`.
    ///
    /// The script is run as `/bin/sh <script>` rather than executed: a file
    /// just written can fail `exec` with `ETXTBSY` while another test
    /// thread's forked child still holds the write descriptor.
    fn fake_monitor(dir: &Path, tail: &str) -> MonitorStream {
        let script_path = dir.join("neuron-monitor");
        let runs = dir.join("runs");
        let runs = runs.display();
        let script = format!("echo run >> '{runs}'\necho '{MONITOR_WITH_RUNTIME}'\n{tail}\n");
        std::fs::write(&script_path, script).unwrap();
        MonitorStream::with_args("/bin/sh", vec![script_path.into_os_string()])
    }

    fn run_count(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("runs"))
            .map(|runs| runs.lines().count())
            .unwrap_or(0)
    }

    /// Wait, bounded, for the reader thread to finish.
    fn wait_until_stopped(shared: &MonitorShared) -> bool {
        let state = shared.lock();
        let (state, _) = shared
            .changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| state.running)
            .unwrap_or_else(|e| e.into_inner());
        !state.running
    }

    #[test]
    fn one_child_serves_many_ticks_and_is_killed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        // `exec` so the script becomes the sleeping process: killing the
        // child then closes the pipe instead of orphaning a `sleep`.
        let stream = fake_monitor(dir.path(), "exec sleep 60");
        let shared = Arc::clone(&stream.shared);

        // The first call waits for the first record.
        let first = stream.latest(Instant::now()).expect("first record");
        assert_eq!(first.utilization.get(&0).copied(), Some(42.5));

        // Later ticks read the same child's latest record; none is spawned.
        for _ in 0..5 {
            assert!(stream.latest(Instant::now()).is_some());
        }
        assert_eq!(run_count(dir.path()), 1);
        assert!(shared.lock().running);

        drop(stream);
        assert!(wait_until_stopped(&shared), "reader thread did not exit");
        assert!(shared.lock().child.is_none(), "child was not reaped");
    }

    #[test]
    fn an_exited_child_is_respawned_only_after_the_back_off() {
        let dir = tempfile::tempdir().unwrap();
        // Prints one record and exits, like a monitor that was killed.
        let stream = fake_monitor(dir.path(), "exit 0");

        assert!(stream.latest(Instant::now()).is_some());
        assert!(wait_until_stopped(&stream.shared));
        assert_eq!(run_count(dir.path()), 1);

        // Inside the back-off the last record is still served and no new
        // child is started.
        let now = Instant::now();
        assert!(stream.shared.backoff_active(now));
        assert!(stream.latest(now).is_some());
        assert_eq!(run_count(dir.path()), 1);

        // Once the back-off has passed, the next call respawns it and
        // utilization recovers. `latest` does not wait this time, so wait
        // for the respawned child to finish.
        stream.latest(now + MONITOR_RETRY_INTERVAL);
        assert!(wait_until_stopped(&stream.shared));
        assert_eq!(run_count(dir.path()), 2);
        assert!(stream.latest(Instant::now()).is_some());
    }

    #[test]
    fn a_missing_binary_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let stream = MonitorStream::new(dir.path().join("absent"));
        assert!(stream.latest(Instant::now()).is_none());
        assert!(!stream.shared.lock().running);
    }
}
