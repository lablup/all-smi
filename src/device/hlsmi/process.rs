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

use crate::utils::command::new_command;
use std::io::BufRead;
use std::io::BufReader;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::config::{HlsmiConfig, ReaderCommand};
use super::store::MetricsStore;

#[cfg(unix)]
use libc;

/// Manages the hl-smi subprocess lifecycle
pub struct ProcessManager {
    process: Arc<Mutex<Option<Child>>>,
    command_tx: Option<Sender<ReaderCommand>>,
    is_running: Arc<Mutex<bool>>,
    config: HlsmiConfig,
    store: Arc<MetricsStore>,
    /// Wakes the monitor thread out of its interval wait on shutdown.
    monitor_shutdown_tx: Option<Sender<()>>,
    /// Joined by `shutdown`, so the thread is gone before it returns.
    monitor_handle: Option<JoinHandle<()>>,
}

impl ProcessManager {
    /// Create a new ProcessManager
    pub fn new(config: HlsmiConfig, store: Arc<MetricsStore>) -> Self {
        Self {
            process: Arc::new(Mutex::new(None)),
            command_tx: None,
            is_running: Arc::new(Mutex::new(false)),
            config,
            store,
            monitor_shutdown_tx: None,
            monitor_handle: None,
        }
    }

    /// Start the hl-smi process and monitoring thread
    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (command_tx, command_rx) = mpsc::channel();
        self.command_tx = Some(command_tx);

        // The panic hook that kills the child is installed once per process
        // by the manager (`manager::install_panic_hook`), not here: wrapping
        // the hook on every start grew the chain by one layer per cycle.

        // Start the hl-smi process
        self.start_hlsmi_process(command_rx)?;

        // Start monitoring thread
        self.start_monitor_thread();

        Ok(())
    }

    /// Start the hl-smi subprocess with stdout piping
    fn start_hlsmi_process(
        &self,
        command_rx: Receiver<ReaderCommand>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd = new_command("hl-smi");

        let args = self.config.get_hlsmi_args();
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        // On Unix, create a new process group so we can kill all children
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;

        // Start reader thread with panic catching
        let store = self.store.clone();
        thread::spawn(move || {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                Self::reader_thread(stdout, &store, command_rx);
            }));
        });

        let mut process_guard = self.process.lock().unwrap();
        *process_guard = Some(child);

        let mut is_running = self.is_running.lock().unwrap();
        *is_running = true;

        Ok(())
    }

    /// Reader thread that processes stdout from hl-smi
    /// hl-smi outputs CSV lines continuously, we accumulate a complete snapshot
    fn reader_thread(
        stdout: std::process::ChildStdout,
        store: &MetricsStore,
        command_rx: Receiver<ReaderCommand>,
    ) {
        use std::fmt::Write;

        let reader = BufReader::new(stdout);
        let mut current_snapshot = String::with_capacity(4096);
        let mut device_count = 0; // 0 means we don't know yet
        let mut lines_in_snapshot = 0;

        for line in reader.lines() {
            // Check for shutdown command
            if let Ok(ReaderCommand::Shutdown) = command_rx.try_recv() {
                break;
            }

            let line = match line {
                Ok(l) => l,
                Err(_) => break, // Pipe broken, process died
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Parse the device index (first field in CSV)
            let device_index = line
                .split(',')
                .next()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(usize::MAX);

            // If we see index 0 and we already have data, this marks the start of a new snapshot
            if device_index == 0 && lines_in_snapshot > 0 {
                // Store the complete snapshot we just finished
                if !current_snapshot.is_empty() {
                    store.push_section(std::mem::take(&mut current_snapshot));
                    current_snapshot.reserve(4096);
                }

                // If we didn't know device count, now we do
                if device_count == 0 {
                    device_count = lines_in_snapshot;
                }

                // Reset for new snapshot
                lines_in_snapshot = 0;
            }

            // Add line to current snapshot
            let _ = writeln!(current_snapshot, "{line}");
            lines_in_snapshot += 1;

            // If we know device count and have collected all devices, store it immediately
            // This handles cases where output comes in batches
            if device_count > 0 && lines_in_snapshot >= device_count {
                store.push_section(std::mem::take(&mut current_snapshot));
                current_snapshot.reserve(4096);
                lines_in_snapshot = 0;
            }
        }
    }

    /// Start a background thread to monitor the process.
    ///
    /// The thread waits on a shutdown channel between checks instead of
    /// sleeping, so `shutdown` can wake it and join it (issue #501).
    fn start_monitor_thread(&mut self) {
        let process_arc = self.process.clone();
        let store_arc = self.store.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let handle = thread::spawn(move || {
            let interval = Duration::from_secs(config.monitor_interval_secs);
            // Any other outcome is a shutdown signal or a dropped sender.
            while let Err(RecvTimeoutError::Timeout) = shutdown_rx.recv_timeout(interval) {
                if !*is_running.lock().unwrap_or_else(|e| e.into_inner()) {
                    break;
                }

                let should_restart = {
                    let mut process_guard = process_arc.lock().unwrap();
                    if let Some(ref mut child) = *process_guard {
                        match child.try_wait() {
                            Ok(Some(_)) => {
                                // Process has exited, need to restart
                                #[cfg(debug_assertions)]
                                eprintln!("hl-smi process died, restarting...");
                                true
                            }
                            Ok(None) => false, // Still running
                            Err(_e) => {
                                #[cfg(debug_assertions)]
                                eprintln!("Error checking hl-smi status: {_e}");
                                true
                            }
                        }
                    } else {
                        false
                    }
                };

                if should_restart {
                    if let Ok(running) = is_running.lock()
                        && !*running
                    {
                        break; // Manager is shutting down
                    }

                    // Create new channel for the restarted process
                    let (_new_tx, new_rx) = mpsc::channel();

                    // Restart hl-smi
                    if let Err(_e) = Self::restart_hlsmi(&process_arc, &store_arc, new_rx, &config)
                    {
                        #[cfg(debug_assertions)]
                        eprintln!("Failed to restart hl-smi: {_e}");
                    }
                }
            }
        });
        self.monitor_shutdown_tx = Some(shutdown_tx);
        self.monitor_handle = Some(handle);
    }

    /// Restart the hl-smi process
    fn restart_hlsmi(
        process_arc: &Arc<Mutex<Option<Child>>>,
        store: &Arc<MetricsStore>,
        command_rx: Receiver<ReaderCommand>,
        config: &HlsmiConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Kill existing process if any
        {
            let mut process_guard = process_arc.lock().unwrap();
            if let Some(mut child) = process_guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        let mut cmd = new_command("hl-smi");
        let args = config.get_hlsmi_args();
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        // On Unix, create a new process group so we can kill all children
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;

        // Start new reader thread with panic catching
        let store = store.clone();
        thread::spawn(move || {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                Self::reader_thread(stdout, &store, command_rx);
            }));
        });

        let mut process_guard = process_arc.lock().unwrap();
        *process_guard = Some(child);

        Ok(())
    }

    /// Shutdown the process manager
    pub fn shutdown(&mut self) {
        // Mark as not running
        {
            let mut is_running = self.is_running.lock().unwrap();
            *is_running = false;
        }

        // Send shutdown command to reader thread
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(ReaderCommand::Shutdown);
        }

        // Wake the monitor and wait for it before touching the child, so a
        // restart it is in the middle of cannot leave a new child behind.
        if let Some(tx) = self.monitor_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.monitor_handle.take()
            && handle.thread().id() != thread::current().id()
        {
            // Never self-join: if shutdown ever runs on the monitor thread
            // itself, the signal above already ends its loop.
            let _ = handle.join();
        }

        // Kill only the process we started
        {
            let mut process_guard = self.process.lock().unwrap();
            if let Some(mut child) = process_guard.take() {
                #[cfg(unix)]
                {
                    // Kill the process group we created
                    let pid = child.id() as i32;
                    unsafe {
                        // Since we set process_group(0) when spawning,
                        // the child is the leader of its own process group
                        let _ = libc::killpg(pid, libc::SIGTERM);
                        thread::sleep(Duration::from_millis(100));

                        // If still running, force kill
                        let _ = libc::killpg(pid, libc::SIGKILL);
                    }
                }

                // Also try to kill via the Child handle
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// Kill the child from the panic hook.
    ///
    /// Uses `try_lock` only: the panicking thread may hold either lock, and
    /// blocking inside a panic hook would deadlock the process.
    pub(super) fn kill_child_on_panic(&self) {
        if let Ok(mut guard) = self.process.try_lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut running) = self.is_running.try_lock() {
            *running = false;
        }
    }

    /// Check if the process is running (test use only)
    #[cfg(test)]
    pub(super) fn is_running(&self) -> bool {
        *self.is_running.lock().unwrap()
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #501: the monitor used to sleep forever once the child was
    /// gone, leaking one thread per `AllSmi` lifecycle.
    #[test]
    fn shutdown_joins_the_monitor_thread() {
        let config = HlsmiConfig::default();
        assert!(config.monitor_interval_secs >= 5);
        let mut manager = ProcessManager::new(config, Arc::new(MetricsStore::new()));
        *manager.is_running.lock().unwrap() = true;
        manager.start_monitor_thread();
        assert!(manager.monitor_handle.is_some());

        let started = std::time::Instant::now();
        manager.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "shutdown waited {:?} for the monitor",
            started.elapsed()
        );
        assert!(manager.monitor_handle.is_none(), "monitor was not joined");
        assert!(!manager.is_running());
    }

    #[test]
    fn monitor_exits_when_marked_not_running() {
        let config = HlsmiConfig {
            monitor_interval_secs: 0,
            ..HlsmiConfig::default()
        };
        let mut manager = ProcessManager::new(config, Arc::new(MetricsStore::new()));
        manager.start_monitor_thread();
        let handle = manager.monitor_handle.take().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(handle.is_finished(), "monitor kept running while stopped");
    }

    #[test]
    fn test_reader_thread_shutdown() {
        use std::io::Cursor;

        let test_input = "0, UUID-1, HL-325L, 1.22.1, 131072 MiB, 672 MiB, 130400 MiB, 226 W, 850 W, 36 C, 0 %\n\
                          1, UUID-2, HL-325L, 1.22.1, 131072 MiB, 672 MiB, 130400 MiB, 230 W, 850 W, 39 C, 0 %\n";
        let cursor = Cursor::new(test_input);

        let data_buffer = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let (tx, rx) = mpsc::channel::<ReaderCommand>();

        // Spawn a thread simulating the reader
        let buffer_clone = data_buffer.clone();
        let handle = thread::spawn(move || {
            let reader = BufReader::new(cursor);
            let mut snapshot = String::new();
            for line in reader.lines() {
                // Check for shutdown
                if let Ok(ReaderCommand::Shutdown) = rx.try_recv() {
                    break;
                }

                if let Ok(line) = line {
                    snapshot.push_str(&line);
                    snapshot.push('\n');
                }

                thread::sleep(Duration::from_millis(10));
            }

            if !snapshot.is_empty() {
                let mut buffer = buffer_clone.lock().unwrap();
                buffer.push_back(snapshot);
            }
        });

        // Let it read some lines
        thread::sleep(Duration::from_millis(50));

        // Send shutdown command
        let _ = tx.send(ReaderCommand::Shutdown);

        // Wait for thread to finish
        let _ = handle.join();

        // The thread completed
        // No assertions needed - test passes if no panic
    }
}
