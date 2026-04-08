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

use crossterm::{
    cursor, queue,
    style::Print,
    terminal::{ClearType, size},
};
use std::io::{Write, stdout};

pub struct BufferWriter {
    buffer: String,
    line_count: usize,
}

impl Default for BufferWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferWriter {
    pub fn new() -> Self {
        Self {
            // Pre-allocate 64KB - sufficient for typical terminal content
            // while avoiding excessive memory usage
            buffer: String::with_capacity(64 * 1024),
            line_count: 0,
        }
    }

    /// Reset the buffer for reuse, keeping the allocated capacity.
    #[allow(dead_code)] // Public API for frame-to-frame buffer reuse
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.line_count = 0;
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }
}

impl Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = std::str::from_utf8(buf)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid UTF-8"))?;

        // Count newlines in the new content
        self.line_count += s.matches('\n').count();

        self.buffer.push_str(s);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Differential renderer that only updates changed lines to eliminate flickering.
///
/// The renderer accepts pre-built frame content and emits only the terminal
/// escape sequences needed to bring the screen from the previous state to the
/// new one. Unchanged lines are skipped entirely.
///
/// Terminal dimensions are accepted from the caller to avoid redundant
/// `terminal::size()` syscalls. The per-line comparison is the authoritative
/// change-detection mechanism; identical lines are skipped via cheap
/// pointer+length string comparison, so duplicate frames incur near-zero cost.
///
/// Terminal writes are collected into an intermediate buffer so that
/// `stdout.write_all + flush` can be offloaded to a blocking thread,
/// preventing slow pipes (e.g. SSH) from stalling the async event loop.
pub struct DifferentialRenderer {
    previous_lines: Vec<String>,
    screen_height: usize,
    screen_width: usize,
    /// Reusable buffer for building terminal escape sequences before flushing.
    write_buf: Vec<u8>,
}

impl DifferentialRenderer {
    pub fn new() -> std::io::Result<Self> {
        let (width, height) = size().unwrap_or((80, 24));
        Ok(Self {
            previous_lines: Vec::new(),
            screen_height: height as usize,
            screen_width: width as usize,
            write_buf: Vec::with_capacity(32 * 1024),
        })
    }

    /// Update screen dimensions. Called by the UI loop when a resize event
    /// occurs, so `render_differential` no longer needs to query the OS.
    pub fn update_dimensions(&mut self, width: u16, height: u16) {
        let w = width as usize;
        let h = height as usize;
        if w != self.screen_width || h != self.screen_height {
            self.screen_width = w;
            self.screen_height = h;
            self.previous_lines.resize(h, String::new());
        }
    }

    /// Render content with differential updates - only changed lines are updated.
    ///
    /// Returns `Some(Vec<u8>)` containing the terminal escape bytes to write,
    /// or `None` if no lines changed. The caller is responsible for writing
    /// the bytes to stdout — this allows offloading the potentially-blocking
    /// write+flush to `spawn_blocking` on slow pipes (SSH).
    pub fn render_differential(
        &mut self,
        content: &str,
        cols: u16,
        rows: u16,
    ) -> std::io::Result<Option<Vec<u8>>> {
        // Keep dimensions in sync with what the caller sees.
        // This is a no-op when dimensions have not changed.
        self.update_dimensions(cols, rows);

        // Initialize previous_lines on first run
        if self.previous_lines.is_empty() {
            self.previous_lines = vec![String::new(); self.screen_height];
        }

        // Build escape sequences into an intermediate buffer so the
        // subsequent write+flush to stdout is a single bulk operation.
        self.write_buf.clear();
        let mut any_changes = false;
        let mut current_line_count = 0;

        // Process lines directly from iterator, updating previous_lines in-place.
        // Per-line string comparison is the authoritative change-detection mechanism.
        // Rust's String `!=` checks length first, so identical lines are O(1).
        for (line_num, current_line) in content.lines().enumerate() {
            if line_num >= self.screen_height {
                break;
            }
            current_line_count = line_num + 1;

            // Check if this line has changed (cheap pointer + length comparison first)
            if self.previous_lines[line_num] != current_line {
                // Update this line - clear it first to prevent artifacts from shorter lines
                queue!(
                    self.write_buf,
                    cursor::MoveTo(0, line_num as u16),
                    crossterm::terminal::Clear(ClearType::UntilNewLine),
                    Print(current_line)
                )?;

                // Update previous_lines in-place, reusing String allocation when possible
                self.previous_lines[line_num].clear();
                self.previous_lines[line_num].push_str(current_line);
                any_changes = true;
            }
        }

        // Clear any remaining lines if the new content is shorter
        for line_num in current_line_count..self.screen_height {
            if !self.previous_lines[line_num].is_empty() {
                queue!(
                    self.write_buf,
                    cursor::MoveTo(0, line_num as u16),
                    crossterm::terminal::Clear(ClearType::CurrentLine)
                )?;
                self.previous_lines[line_num].clear();
                any_changes = true;
            }
        }

        if any_changes {
            Ok(Some(self.write_buf.clone()))
        } else {
            Ok(None)
        }
    }

    /// Synchronously write pre-built terminal bytes to stdout and flush.
    /// This is the only blocking I/O operation and can be called from
    /// `spawn_blocking` to avoid stalling the async event loop.
    pub fn flush_to_stdout(buf: &[u8]) -> std::io::Result<()> {
        let mut stdout = stdout();
        stdout.write_all(buf)?;
        stdout.flush()
    }

    /// Force clear the entire screen (use sparingly, e.g., on startup or resize)
    pub fn force_clear(&mut self) -> std::io::Result<()> {
        let mut stdout = stdout();
        queue!(stdout, crossterm::terminal::Clear(ClearType::All))?;
        stdout.flush()?;

        // Reset previous state to force re-render
        self.previous_lines.clear();
        self.previous_lines
            .resize(self.screen_height, String::new());

        Ok(())
    }
}
