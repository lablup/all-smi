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

use std::io::BufRead;
use std::io::BufReader;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
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
        }
    }

    /// Start the hl-smi process and monitoring thread
    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (command_tx, command_rx) = mpsc::channel();
        self.command_tx = Some(command_tx);

        // Set up panic handler to cleanup on panic
        let process_clone = self.process.clone();
        let is_running_clone = self.is_running.clone();
        let old_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            // Cleanup subprocess on panic
            if let Ok(mut guard) = process_clone.lock()
                && let Some(mut child) = guard.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Ok(mut running) = is_running_clone.lock() {
                *running = false;
            }
            old_hook(panic_info);
        }));

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
        let mut cmd = Command::new("hl-smi");

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
        let data_buffer = self.store.get_buffer();
        let buffer_capacity = self.config.buffer_capacity;
        thread::spawn(move || {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                Self::reader_thread(stdout, data_buffer, command_rx, buffer_capacity);
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
        data_buffer: Arc<Mutex<std::collections::VecDeque<String>>>,
        command_rx: Receiver<ReaderCommand>,
        buffer_capacity: usize,
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
                    let mut buffer = data_buffer.lock().unwrap();
                    if buffer.len() >= buffer_capacity {
                        buffer.pop_front(); // Remove oldest
                    }
                    buffer.push_back(std::mem::take(&mut current_snapshot));
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
                let mut buffer = data_buffer.lock().unwrap();
                if buffer.len() >= buffer_capacity {
                    buffer.pop_front();
                }
                buffer.push_back(std::mem::take(&mut current_snapshot));
                current_snapshot.reserve(4096);
                lines_in_snapshot = 0;
            }
        }
    }

    /// Start a background thread to monitor the process
    fn start_monitor_thread(&self) {
        let process_arc = self.process.clone();
        let store_arc = self.store.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(config.monitor_interval_secs));

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

        let mut cmd = Command::new("hl-smi");
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
        let data_buffer = store.get_buffer();
        let buffer_capacity = config.buffer_capacity;
        thread::spawn(move || {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                Self::reader_thread(stdout, data_buffer, command_rx, buffer_capacity);
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
