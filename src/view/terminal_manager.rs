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

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

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
            let mut stdout = stdout();
            // Leave alternate screen and restore terminal state
            let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
            let _ = disable_raw_mode();
            // No "Terminating..." message needed - native APIs don't require cleanup
        }
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self { initialized: false })
    }
}
