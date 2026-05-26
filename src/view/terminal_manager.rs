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

use std::io::stdout;
use std::sync::{
    Once,
    atomic::{AtomicBool, Ordering},
};

use crossterm::{
    cursor,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

/// Process-global flag: `true` when the terminal has been set to alternate-screen /
/// raw mode and the cursor has been hidden. Any exit path — normal `q`, SIGINT,
/// SIGTERM, or panic — must call `restore_terminal()` to clear this flag and
/// emit the cleanup escape sequences.
///
/// Initialised `false` so `restore_terminal()` is a no-op for subcommands that
/// never touch the TUI (e.g., `snapshot`, `doctor`).
static TERMINAL_NEEDS_RESTORE: AtomicBool = AtomicBool::new(false);

/// `Once` guard that ensures the panic hook is installed exactly once, even if
/// `TerminalManager::new()` were called multiple times within the same process.
static PANIC_HOOK_INSTALLED: Once = Once::new();

pub struct TerminalManager {
    initialized: bool,
}

impl TerminalManager {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut manager = Self { initialized: false };
        manager.initialize()?;
        Ok(manager)
    }

    fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if enable_raw_mode().is_err() {
            return Err("Failed to enable raw mode - terminal not available".into());
        }

        let mut stdout = stdout();
        if execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            crossterm::terminal::Clear(ClearType::All)
        )
        .is_err()
        {
            let _ = disable_raw_mode();
            return Err("Failed to initialize terminal display".into());
        }

        // Mark the terminal as needing restoration on every exit path.
        // This must be set before `self.initialized = true` so that if any
        // subsequent initialisation step panics the flag is already live.
        TERMINAL_NEEDS_RESTORE.store(true, Ordering::SeqCst);

        // Install the panic hook exactly once. `take_hook` captures whatever is
        // on top of the panic-hook stack at this point — which may be the macOS
        // native-metrics hook installed earlier by `setup_panic_handlers` in
        // main.rs — so the chain is preserved correctly.
        PANIC_HOOK_INSTALLED.call_once(install_panic_hook);

        self.initialized = true;
        Ok(())
    }

    #[allow(dead_code)] // Future terminal management architecture
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        if self.initialized {
            // Restore cursor visibility in addition to leaving the alternate screen.
            // `LeaveAlternateScreen` alone does NOT guarantee cursor visibility on
            // Linux terminals (VTE family, tmux, kitty, …) that track cursor state
            // independently of the alternate-screen mode — hence the explicit
            // `cursor::Show` (issue #235).
            restore_terminal();
        }
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self { initialized: false })
    }
}

/// Restore the terminal to a usable state: show the cursor, leave the alternate
/// screen, disable mouse capture, and disable raw mode.
///
/// This function is **idempotent**: the first call after `TerminalManager::new()`
/// performs the cleanup; any subsequent call is a silent no-op. It is therefore
/// safe to call from `Drop`, from SIGINT/SIGTERM handlers, and from a panic hook
/// simultaneously without double-emitting escape sequences.
///
/// All errors are intentionally ignored with `let _ = ...`. At cleanup time we
/// cannot meaningfully recover from a write failure (the terminal may already be
/// in a broken state), and panicking inside a signal handler or a panic hook
/// would abort the process uncleanly.
pub fn restore_terminal() {
    // Atomically clear the flag. If it was already `false` (either because the
    // terminal was never initialised, or because a concurrent call got here
    // first), return immediately — nothing to do.
    if !TERMINAL_NEEDS_RESTORE.swap(false, Ordering::SeqCst) {
        return;
    }

    // Use `std::io::stdout()` directly. No *application-owned* locks are held
    // across this call — `execute!` acquires the stdlib stdout mutex internally
    // and releases it before returning, which is fine in every context this
    // function can be called from (panic hook, tokio task, `Drop`).
    let mut stdout = std::io::stdout();
    let _ = execute!(
        stdout,
        cursor::Show,
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = disable_raw_mode();
}

/// Install a panic hook that calls `restore_terminal()` before delegating to
/// whatever hook was previously installed (either the default backtrace hook, or
/// the macOS native-metrics hook set up by `setup_panic_handlers` in main.rs).
///
/// Layering via `take_hook` / `set_hook` is the idiomatic Rust pattern for
/// composing panic hooks; the terminal hook is always outermost after this call
/// so it runs first and the terminal is usable when the inner hook prints its
/// backtrace.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Restore the terminal first so the panic backtrace is readable.
        restore_terminal();
        previous(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // All three tests mutate the process-global `TERMINAL_NEEDS_RESTORE` flag.
    // Cargo may run tests in parallel within a binary, so we serialize them with
    // an in-module mutex rather than adding a new dev-dependency.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Calling `restore_terminal()` when the flag is `false` must be a no-op and
    /// must not panic. We reset the flag explicitly at the start so parallel test
    /// runs (cargo parallelises tests within a binary) see a consistent baseline.
    #[test]
    fn restore_terminal_is_noop_when_flag_unset() {
        let _guard = TEST_MUTEX.lock().unwrap();
        TERMINAL_NEEDS_RESTORE.store(false, Ordering::SeqCst);
        // Neither call should panic.
        restore_terminal();
        restore_terminal();
        assert!(!TERMINAL_NEEDS_RESTORE.load(Ordering::SeqCst));
    }

    /// Manually setting the flag to `true` and then calling `restore_terminal()`
    /// must atomically clear it to `false`. We do not assert on the IO output
    /// (escape codes go to stdout and may not be readable in a test context).
    #[test]
    fn restore_terminal_clears_flag() {
        let _guard = TEST_MUTEX.lock().unwrap();
        TERMINAL_NEEDS_RESTORE.store(true, Ordering::SeqCst);
        restore_terminal();
        assert!(!TERMINAL_NEEDS_RESTORE.load(Ordering::SeqCst));
    }

    /// Calling `restore_terminal()` repeatedly with the flag initially `true`
    /// must be idempotent: the first call clears the flag, subsequent calls are
    /// silent no-ops, and no call panics.
    #[test]
    fn restore_terminal_is_idempotent() {
        let _guard = TEST_MUTEX.lock().unwrap();
        TERMINAL_NEEDS_RESTORE.store(true, Ordering::SeqCst);
        restore_terminal();
        restore_terminal();
        restore_terminal();
        assert!(!TERMINAL_NEEDS_RESTORE.load(Ordering::SeqCst));
    }
}
