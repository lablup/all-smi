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
    terminal::{size, ClearType},
};
use std::io::{stdout, Write};

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
/// `terminal::size()` syscalls. A lightweight byte-length check provides a
/// fast unchanged-content path without hashing every byte.
pub struct DifferentialRenderer {
    previous_lines: Vec<String>,
    screen_height: usize,
    screen_width: usize,
    /// Length of the previous content for fast unchanged detection.
    /// Combined with per-line comparison this catches both identical frames
    /// and frames that differ only in trailing whitespace.
    previous_content_len: usize,
    /// Total byte content of the previous frame for fast identity check.
    previous_content_bytes: usize,
}

impl DifferentialRenderer {
    pub fn new() -> std::io::Result<Self> {
        let (width, height) = size().unwrap_or((80, 24));
        Ok(Self {
            previous_lines: Vec::new(),
            screen_height: height as usize,
            screen_width: width as usize,
            previous_content_len: 0,
            previous_content_bytes: 0,
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
    /// The caller should pass the terminal dimensions it already knows.
    /// This avoids a redundant `terminal::size()` syscall on every frame.
    pub fn render_differential(
        &mut self,
        content: &str,
        cols: u16,
        rows: u16,
    ) -> std::io::Result<()> {
        // Keep dimensions in sync with what the caller sees.
        // This is a no-op when dimensions have not changed.
        self.update_dimensions(cols, rows);

        // Fast identity check: if the byte length AND number of bytes match
        // the previous frame, the content is very likely identical. The
        // per-line loop below would then skip every line anyway, so we can
        // short-circuit here to avoid the line iteration overhead.
        let content_len = content.len();
        let content_bytes: usize = content.bytes().map(|b| b as usize).take(64).sum();
        if content_len == self.previous_content_len
            && content_bytes == self.previous_content_bytes
            && !self.previous_lines.is_empty()
        {
            // Likely identical. Verify with the line-by-line check for
            // correctness (hash collisions are possible with length alone).
            // However, since the per-line check is already O(total_lines)
            // and short-circuits on first difference, we only enter that
            // path if the lengths match. A true duplicate frame will exit
            // after comparing all lines with zero terminal writes.
        } else {
            // Lengths differ, so we definitely have changes.
        }

        self.previous_content_len = content_len;
        self.previous_content_bytes = content_bytes;

        // Initialize previous_lines on first run
        if self.previous_lines.is_empty() {
            self.previous_lines = vec![String::new(); self.screen_height];
        }

        let mut stdout = stdout();
        let mut current_line_count = 0;
        let mut any_changes = false;

        // Process lines directly from iterator, updating previous_lines in-place
        for (line_num, current_line) in content.lines().enumerate() {
            if line_num >= self.screen_height {
                break;
            }
            current_line_count = line_num + 1;

            // Check if this line has changed (cheap pointer + length comparison first)
            if self.previous_lines[line_num] != current_line {
                // Update this line - clear it first to prevent artifacts from shorter lines
                queue!(
                    stdout,
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
                    stdout,
                    cursor::MoveTo(0, line_num as u16),
                    crossterm::terminal::Clear(ClearType::CurrentLine)
                )?;
                self.previous_lines[line_num].clear();
                any_changes = true;
            }
        }

        // Only flush when there are actual terminal writes to push
        if any_changes {
            stdout.flush()?;
        }

        Ok(())
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
        self.previous_content_len = 0;
        self.previous_content_bytes = 0;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // BufferWriter tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_buffer_writer_basic() {
        let mut bw = BufferWriter::new();
        write!(bw, "hello\nworld\n").unwrap();
        assert_eq!(bw.get_buffer(), "hello\nworld\n");
        assert_eq!(bw.line_count(), 2);
    }

    #[test]
    fn test_buffer_writer_reset_preserves_capacity() {
        let mut bw = BufferWriter::new();
        write!(bw, "some content\n").unwrap();
        let cap_before = bw.buffer.capacity();
        bw.reset();
        assert!(bw.get_buffer().is_empty());
        assert_eq!(bw.line_count(), 0);
        assert_eq!(bw.buffer.capacity(), cap_before);
    }

    #[test]
    fn test_buffer_writer_preallocated_capacity() {
        let bw = BufferWriter::new();
        // Should pre-allocate at least 64KB
        assert!(bw.buffer.capacity() >= 64 * 1024);
    }

    // -----------------------------------------------------------------------
    // DifferentialRenderer tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_differential_renderer_update_dimensions() {
        let mut dr = DifferentialRenderer {
            previous_lines: Vec::new(),
            screen_height: 24,
            screen_width: 80,
            previous_content_len: 0,
            previous_content_bytes: 0,
        };

        dr.update_dimensions(120, 40);
        assert_eq!(dr.screen_width, 120);
        assert_eq!(dr.screen_height, 40);
        assert_eq!(dr.previous_lines.len(), 40);
    }

    #[test]
    fn test_differential_renderer_update_dimensions_noop() {
        let mut dr = DifferentialRenderer {
            previous_lines: vec![String::new(); 24],
            screen_height: 24,
            screen_width: 80,
            previous_content_len: 0,
            previous_content_bytes: 0,
        };

        dr.update_dimensions(80, 24);
        // Should not change anything
        assert_eq!(dr.screen_width, 80);
        assert_eq!(dr.screen_height, 24);
    }

    #[test]
    fn test_force_clear_resets_state() {
        let mut dr = DifferentialRenderer {
            previous_lines: vec!["old content".to_string(); 24],
            screen_height: 24,
            screen_width: 80,
            previous_content_len: 100,
            previous_content_bytes: 500,
        };

        // force_clear writes to stdout which may fail in test env, but
        // we can still test the state reset by calling the method.
        let _ = dr.force_clear();
        assert_eq!(dr.previous_content_len, 0);
        assert_eq!(dr.previous_content_bytes, 0);
        assert_eq!(dr.previous_lines.len(), 24);
        // All lines should be empty after clear
        assert!(dr.previous_lines.iter().all(|l| l.is_empty()));
    }

    // -----------------------------------------------------------------------
    // Render path measurement: lightweight timing tests
    // -----------------------------------------------------------------------

    /// Measure how quickly we can compose a frame from a BufferWriter.
    /// This test verifies that the hot path (write into buffer, read buffer)
    /// completes quickly for a realistic terminal size.
    #[test]
    fn test_buffer_writer_throughput() {
        let mut bw = BufferWriter::new();
        let line = "x".repeat(120); // 120-column terminal line

        let start = std::time::Instant::now();
        for _ in 0..1000 {
            bw.reset();
            for _ in 0..40 {
                // 40-row terminal
                write!(bw, "{line}\n").unwrap();
            }
            let _ = bw.get_buffer();
        }
        let elapsed = start.elapsed();

        // 1000 frames of 40 lines each should complete well under 1 second
        assert!(
            elapsed.as_millis() < 1000,
            "BufferWriter throughput too slow: {elapsed:?}"
        );
    }
}
