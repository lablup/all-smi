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

use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::io::Write;

use crossterm::{queue, style::Color, style::Print};

use crate::device::ProcessInfo;
use crate::ui::text::{print_colored_text, truncate_to_width};

/// Reusable formatting scratch buffer for the process renderer.
///
/// Keeping this in a struct avoids re-allocating on every process row.
/// The buffer is cleared between rows but its capacity is retained.
struct RowFormatter {
    buf: String,
}

impl RowFormatter {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(512),
        }
    }

    /// Clear the buffer, keeping allocated capacity.
    fn clear(&mut self) {
        self.buf.clear();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn print_process_info<W: Write>(
    stdout: &mut W,
    processes: &[ProcessInfo],
    selected_index: usize,
    start_index: usize,
    available_rows: u16,
    cols: u16,
    horizontal_scroll_offset: usize,
    current_user: &str,
    sort_criteria: &crate::app_state::SortCriteria,
    sort_direction: &crate::app_state::SortDirection,
) {
    // Don't add extra newlines at the start - the caller should handle positioning
    let width = cols as usize;

    // Styled title line: "── Processes ───────────────────────"
    print_colored_text(stdout, "\u{2500}\u{2500} ", Color::Cyan, None, None);
    print_colored_text(stdout, "Processes", Color::Cyan, None, None);
    print_colored_text(stdout, " ", Color::DarkGrey, None, None);
    let title_prefix_cols = 3 + "Processes".len() + 1; // "── Processes "
    let dashes = width.saturating_sub(title_prefix_cols);
    print_colored_text(
        stdout,
        &"\u{2500}".repeat(dashes),
        Color::DarkGrey,
        None,
        None,
    );
    queue!(stdout, Print("\r\n")).unwrap();

    // Fixed column widths based on actual data sizes
    // PID: 7 (up to 9999999), USER: 12, PRI: 3, NI: 3, VIRT: 6, RES: 6, S: 1,
    // CPU%: 5, MEM%: 5, GPU%: 5, VRAM: 7, TIME+: 10, Command: remaining
    //
    // NOTE: TIME+ must be wide enough to hold the longest value produced by
    // `format_cpu_time`, which is "HHHH:MM:SS" (10 chars) at the 365-day cap
    // (8760 hours). Using a smaller width causes 9- or 10-char values like
    // "213:16:04" to overflow and push the Command column out of alignment —
    // the format specifier `{:>N}` is a minimum, not a maximum.
    let fixed_widths = [7, 12, 3, 3, 6, 6, 1, 5, 5, 5, 7, 10];
    let num_gaps = fixed_widths.len(); // Gaps between columns (not after last column)
    let fixed_total: usize = fixed_widths.iter().sum::<usize>() + num_gaps;

    // Give remaining space to command column, ensure at least 20 chars
    let _command_w = if width > fixed_total + 20 {
        width - fixed_total
    } else {
        20
    };

    let (pid_w, user_w, pri_w, ni_w, virt_w, res_w, s_w, cpu_w, mem_w, gpu_w, gpu_mem_w, time_w) = (
        fixed_widths[0],  // PID: 7
        fixed_widths[1],  // USER: 12
        fixed_widths[2],  // PRI: 3
        fixed_widths[3],  // NI: 3
        fixed_widths[4],  // VIRT: 6
        fixed_widths[5],  // RES: 6
        fixed_widths[6],  // S: 1
        fixed_widths[7],  // CPU%: 5
        fixed_widths[8],  // MEM%: 5
        fixed_widths[9],  // GPU%: 5
        fixed_widths[10], // VRAM: 7
        fixed_widths[11], // TIME+: 10
    );

    // Helper function to add sort arrow
    let get_sort_arrow = |criteria: crate::app_state::SortCriteria| -> &'static str {
        if sort_criteria == &criteria {
            match sort_direction {
                crate::app_state::SortDirection::Ascending => "\u{2191}",
                crate::app_state::SortDirection::Descending => "\u{2193}",
            }
        } else {
            ""
        }
    };

    // Build header format string with proper alignment and sort arrows
    #[allow(clippy::format_in_format_args)]
    let header_format = format!(
        "{:>pid_w$} {:<user_w$} {:>pri_w$} {:>ni_w$} {:>virt_w$} {:>res_w$} {:<s_w$} {:>cpu_w$} {:>mem_w$} {:>gpu_w$} {:>gpu_mem_w$} {:>time_w$} {}",
        format!("PID{}", get_sort_arrow(crate::app_state::SortCriteria::Pid)),
        format!(
            "USER{}",
            get_sort_arrow(crate::app_state::SortCriteria::User)
        ),
        format!(
            "PRI{}",
            get_sort_arrow(crate::app_state::SortCriteria::Priority)
        ),
        format!("NI{}", get_sort_arrow(crate::app_state::SortCriteria::Nice)),
        format!(
            "VIRT{}",
            get_sort_arrow(crate::app_state::SortCriteria::VirtualMemory)
        ),
        format!(
            "RES{}",
            get_sort_arrow(crate::app_state::SortCriteria::ResidentMemory)
        ),
        format!("S{}", get_sort_arrow(crate::app_state::SortCriteria::State)),
        format!(
            "CPU%{}",
            get_sort_arrow(crate::app_state::SortCriteria::CpuPercent)
        ),
        format!(
            "MEM%{}",
            get_sort_arrow(crate::app_state::SortCriteria::MemoryPercent)
        ),
        format!(
            "GPU%{}",
            get_sort_arrow(crate::app_state::SortCriteria::GpuPercent)
        ),
        format!(
            "VRAM{}",
            get_sort_arrow(crate::app_state::SortCriteria::GpuMemoryUsage)
        ),
        format!(
            "TIME+{}",
            get_sort_arrow(crate::app_state::SortCriteria::CpuTime)
        ),
        format!(
            "Command{}",
            get_sort_arrow(crate::app_state::SortCriteria::Command)
        ),
    );

    // Apply horizontal scrolling
    let visible_header = if horizontal_scroll_offset < header_format.len() {
        let scrolled = &header_format[horizontal_scroll_offset..];
        // Pad the header to full width to clear any previous content
        format!("{:<width$}", truncate_to_width(scrolled, width))
    } else {
        // Clear the entire line when scrolled past the content
        " ".repeat(width)
    };

    print_colored_text(stdout, &visible_header, Color::White, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Print separator line
    let separator = "\u{2500}".repeat(width.min(120));
    print_colored_text(stdout, &separator, Color::DarkGrey, None, None);
    queue!(stdout, Print("\r\n")).unwrap();

    // Calculate how many rows are reserved for footer information
    let footer_rows = 2usize; // "Showing..." line + "Active..." stats line

    // Calculate how many processes we can display
    // Reserve rows for header section: 1 for "Processes:" title, 1 for header, 1 for separator, 1 for blank line
    const RESERVED_HEADER_ROWS: usize = 4;
    let available_rows_for_processes =
        (available_rows as usize).saturating_sub(RESERVED_HEADER_ROWS + footer_rows);
    let end_index = (start_index + available_rows_for_processes).min(processes.len());

    // Create a single clear-line string to reuse for empty row fills,
    // avoiding repeated allocations of " ".repeat(width).
    let clear_line = " ".repeat(width);

    // Reusable scratch buffers for per-row formatting
    let mut row_fmt = RowFormatter::new();
    let mut row_buf = String::with_capacity(512);

    // Print process information
    for i in start_index..end_index {
        if let Some(process) = processes.get(i) {
            let is_selected = i == selected_index;

            // Format process information, reusing the scratch buffer.
            // For small fixed-width fields we use write! into row_buf
            // instead of allocating a new String per field.
            row_buf.clear();
            let _ = write!(row_buf, "{}", process.pid);
            let pid: &str = &row_buf;

            let user = truncate_to_width(&process.user, user_w);

            // We need separate small buffers for fields used simultaneously
            // in the row format string.
            let mut priority_buf = String::with_capacity(8);
            let _ = write!(priority_buf, "{}", process.priority);
            let priority: &str = &priority_buf;

            let mut nice_buf = String::with_capacity(8);
            let _ = write!(nice_buf, "{:+}", process.nice_value);

            // Format memory sizes into temporary strings
            let mut virt_buf = String::with_capacity(8);
            write_memory_size(&mut virt_buf, process.memory_vms);
            let mut res_buf = String::with_capacity(8);
            write_memory_size(&mut res_buf, process.memory_rss);

            let state = truncate_to_width(&process.state, s_w);

            let mut cpu_pct_buf = String::with_capacity(8);
            let _ = write!(cpu_pct_buf, "{:.1}", process.cpu_percent);
            let mut mem_pct_buf = String::with_capacity(8);
            let _ = write!(mem_pct_buf, "{:.1}", process.memory_percent);

            // Format GPU utilization -- use Cow to avoid allocation for static strings
            let gpu_percent: Cow<'_, str> = if process.uses_gpu && process.gpu_utilization > 0.0 {
                let mut buf = String::with_capacity(8);
                let _ = write!(buf, "{:.1}", process.gpu_utilization);
                Cow::Owned(buf)
            } else if process.uses_gpu {
                Cow::Borrowed("-")
            } else {
                Cow::Borrowed("")
            };

            // Format GPU memory usage
            let gpu_mem: Cow<'_, str> = if process.used_memory > 0 {
                let gpu_mem_mb = process.used_memory as f64 / (1024.0 * 1024.0);
                let mut buf = String::with_capacity(8);
                if gpu_mem_mb >= 1024.0 {
                    let _ = write!(buf, "{:.1}G", gpu_mem_mb / 1024.0);
                } else {
                    let _ = write!(buf, "{gpu_mem_mb:.0}M");
                }
                Cow::Owned(buf)
            } else if process.uses_gpu {
                Cow::Borrowed("-")
            } else {
                Cow::Borrowed("")
            };

            // Format CPU time
            let time_plus = format_cpu_time(process.cpu_time);

            // Borrow command directly instead of cloning
            let command: &str = &process.command;

            // Build the row with proper formatting and padding.
            // Reuse row_fmt.buf for the full row assembly.
            row_fmt.clear();
            let user_trunc = truncate_to_width(&user, user_w);
            #[allow(clippy::uninlined_format_args)]
            let _ = write!(
                row_fmt.buf,
                "{pid:>pid_w$} {:<user_w$} {priority:>pri_w$} {:>ni_w$} {:>virt_w$} {:>res_w$} {state:<s_w$} {:>cpu_w$} {:>mem_w$} {:>gpu_w$} {:>gpu_mem_w$} {time_plus:>time_w$} {command}",
                user_trunc,
                nice_buf,
                virt_buf,
                res_buf,
                cpu_pct_buf,
                mem_pct_buf,
                gpu_percent,
                gpu_mem,
            );
            let row_format = &row_fmt.buf;

            // Apply horizontal scrolling
            let visible_row: Cow<'_, str> = if horizontal_scroll_offset < row_format.len() {
                let scrolled = &row_format[horizontal_scroll_offset..];
                // Pad the row to full width to clear any previous content
                Cow::Owned(format!("{:<width$}", truncate_to_width(scrolled, width)))
            } else {
                // Reuse the pre-built clear line
                Cow::Borrowed(&clear_line)
            };

            // Print with selection highlight or individual column colors
            if is_selected {
                print_colored_text(stdout, &visible_row, Color::Black, Some(Color::White), None);
            } else {
                // We need to print each column separately with its own color
                // So we'll reconstruct the visible parts column by column
                print_process_row_colored(
                    stdout,
                    process,
                    current_user,
                    pid,
                    &user,
                    priority,
                    &nice_buf,
                    &virt_buf,
                    &res_buf,
                    &state,
                    &cpu_pct_buf,
                    &mem_pct_buf,
                    &gpu_percent,
                    &gpu_mem,
                    &time_plus,
                    command,
                    horizontal_scroll_offset,
                    width,
                    &fixed_widths,
                );
            }

            queue!(stdout, Print("\r\n")).unwrap();
        }
    }

    // Calculate lines used so far
    let mut lines_used = 3; // "Processes:" (1) + header (1) + separator (1)
    lines_used += end_index.saturating_sub(start_index); // actual process lines

    // Fill empty space between processes and footer
    let total_lines_before_footer = (available_rows as usize).saturating_sub(footer_rows);
    while lines_used < total_lines_before_footer {
        queue!(stdout, Print(clear_line.as_str())).unwrap();
        queue!(stdout, Print("\r\n")).unwrap();
        lines_used += 1;
    }

    // Show navigation info if there are more processes
    if processes.len() > available_rows_for_processes {
        let nav_info = format!(
            "Showing {}-{end_index} of {} processes (Use \u{2191}\u{2193} to navigate, PgUp/PgDn for pages)",
            start_index + 1,
            processes.len()
        );
        // Pad the line to full width to clear any previous content
        let padded_nav_info = format!("{nav_info:<width$}");
        print_colored_text(stdout, &padded_nav_info, Color::DarkGrey, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
        lines_used += 1;
    } else if !processes.is_empty() {
        // If all processes fit, still show a summary line
        let nav_info = format!("Showing all {} processes", processes.len());
        let padded_nav_info = format!("{nav_info:<width$}");
        print_colored_text(stdout, &padded_nav_info, Color::DarkGrey, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
        lines_used += 1;
    }

    // Show process statistics
    if !processes.is_empty() {
        let total_gpu_mem: u64 = processes.iter().map(|p| p.used_memory).sum();
        let gpu_mem_gb = total_gpu_mem as f64 / (1024.0 * 1024.0 * 1024.0);

        let active_processes = processes.iter().filter(|p| p.cpu_percent > 0.1).count();
        let gpu_processes = processes.iter().filter(|p| p.used_memory > 0).count();

        let stats = format!(
            "Active: {active_processes} | GPU: {gpu_processes} | Total GPU Memory: {gpu_mem_gb:.1}GB"
        );
        // Pad the line to full width to clear any previous content
        let padded_stats = format!("{stats:<width$}");
        print_colored_text(stdout, &padded_stats, Color::Cyan, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
        lines_used += 1;
    }

    // Fill remaining space up to available_rows, reusing the pre-built clear line
    while lines_used < available_rows as usize {
        queue!(stdout, Print(clear_line.as_str())).unwrap();
        queue!(stdout, Print("\r\n")).unwrap();
        lines_used += 1;
    }
}

/// Write a human-readable memory size into `buf` without allocating.
///
/// Examples: "0", "187T", "123G", "500M", "16K"
fn write_memory_size(buf: &mut String, bytes: u64) {
    if bytes == 0 {
        buf.push('0');
        return;
    }

    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let kb = bytes as f64 / 1024.0;

    if gb >= 1000.0 {
        let tb = gb / 1024.0;
        let _ = write!(buf, "{tb:.0}T");
    } else if gb >= 1.0 {
        let _ = write!(buf, "{gb:.0}G");
    } else if mb >= 1.0 {
        let _ = write!(buf, "{mb:.0}M");
    } else if kb >= 1.0 {
        let _ = write!(buf, "{kb:.0}K");
    } else {
        let _ = write!(buf, "{bytes}");
    }
}

/// Print process row with individual column colors
#[allow(clippy::too_many_arguments)]
fn print_process_row_colored<W: Write>(
    stdout: &mut W,
    process: &ProcessInfo,
    current_user: &str,
    pid: &str,
    user: &str,
    priority: &str,
    nice: &str,
    virt: &str,
    res: &str,
    state: &str,
    cpu_percent: &str,
    mem_percent: &str,
    gpu_percent: &str,
    gpu_mem: &str,
    time_plus: &str,
    command: &str,
    horizontal_scroll_offset: usize,
    width: usize,
    fixed_widths: &[usize; 12],
) {
    let values: [&str; 13] = [
        pid,
        user,
        priority,
        nice,
        virt,
        res,
        state,
        cpu_percent,
        mem_percent,
        gpu_percent,
        gpu_mem,
        time_plus,
        command,
    ];

    let mut current_pos = 0;
    let mut output_pos = 0;

    // Determine base colors
    let is_current_user = process.user == current_user;

    // Determine the default text color based on user and resource usage
    let default_color = if process.cpu_percent >= 90.0 || process.memory_percent >= 90.0 {
        Color::Red
    } else if process.cpu_percent >= 80.0 || process.memory_percent >= 80.0 {
        Color::Rgb {
            r: 255,
            g: 100,
            b: 100,
        }
    } else if process.cpu_percent >= 70.0 || process.memory_percent >= 70.0 {
        Color::Yellow
    } else if process.cpu_percent >= 50.0 || process.memory_percent >= 50.0 {
        Color::Rgb {
            r: 255,
            g: 200,
            b: 0,
        }
    } else if process.uses_gpu && (process.cpu_percent >= 30.0 || process.memory_percent >= 30.0) {
        Color::Cyan
    } else if process.uses_gpu {
        Color::Green
    } else if is_current_user {
        Color::White
    } else {
        // Root, unknown, or other users' processes
        Color::DarkGrey
    };

    // Reusable buffer for per-column formatting to avoid per-column allocations
    let mut col_buf = String::with_capacity(64);

    for (idx, value) in values.iter().enumerate() {
        let col_width = if idx < fixed_widths.len() {
            fixed_widths[idx]
        } else {
            // Command column takes remaining space
            width
                .saturating_sub(current_pos)
                .saturating_sub(horizontal_scroll_offset)
        };

        // Check if this column is visible after scrolling
        let col_start = current_pos;
        let col_end = if idx < fixed_widths.len() {
            current_pos + col_width + 1 // +1 for space
        } else {
            current_pos + value.len() // Command doesn't have fixed width
        };

        if col_end > horizontal_scroll_offset && output_pos < width {
            // Determine color for this column
            let color = match idx {
                4 => {
                    // VIRT column
                    if process.memory_vms == 0 {
                        Color::White
                    } else {
                        Color::Green
                    }
                }
                0 => {
                    // PID - white if non-zero
                    if process.pid > 0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                2 => {
                    // Priority - white if not default (20)
                    if process.priority != 20 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                3 => {
                    // Nice - white if not 0
                    if process.nice_value != 0 {
                        Color::White
                    } else {
                        Color::DarkGrey
                    }
                }
                5 => {
                    // RES - white if non-zero
                    if process.memory_rss > 0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                7 => {
                    // CPU% - white if non-zero
                    if process.cpu_percent > 0.0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                8 => {
                    // MEM% - white if non-zero
                    if process.memory_percent > 0.0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                9 => {
                    // GPU% - white if non-zero
                    if process.gpu_utilization > 0.0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                10 => {
                    // GPUMEM - white if non-zero
                    if process.used_memory > 0 {
                        Color::White
                    } else {
                        default_color
                    }
                }
                11 => {
                    // TIME+ - white if not 0:00:00
                    if time_plus != "0:00:00" {
                        Color::White
                    } else {
                        default_color
                    }
                }
                _ => default_color, // USER, State, Command use default color
            };

            // Calculate what part of this column to display
            let skip = horizontal_scroll_offset.saturating_sub(col_start);

            // Format the value with proper alignment into the reusable buffer
            col_buf.clear();
            if idx < fixed_widths.len() {
                match idx {
                    0 => {
                        let _ = write!(col_buf, "{value:>col_width$}");
                    }
                    1 => {
                        let truncated = truncate_to_width(value, col_width);
                        let _ = write!(col_buf, "{truncated:<col_width$}");
                    }
                    2..=11 => {
                        let _ = write!(col_buf, "{value:>col_width$}");
                    }
                    _ => col_buf.push_str(value),
                }
            } else {
                col_buf.push_str(value); // Command - no padding
            }

            // Print the visible part
            if skip < col_buf.len() {
                let visible_part = &col_buf[skip..];
                let remaining_width = width.saturating_sub(output_pos);
                let to_print = truncate_to_width(visible_part, remaining_width);
                print_colored_text(stdout, &to_print, color, None, None);
                output_pos += to_print.len();
            }

            // Add space between columns (except after last column)
            if idx < values.len() - 1 && output_pos < width && col_end > horizontal_scroll_offset {
                print_colored_text(stdout, " ", default_color, None, None);
                output_pos += 1;
            }
        }

        current_pos = col_end;
    }

    // Fill the rest of the line with spaces to clear any previous content
    if output_pos < width {
        let pad_len = width - output_pos;
        // For small padding, use a static string to avoid allocation
        let padding: Cow<'_, str> = match pad_len {
            0 => Cow::Borrowed(""),
            1 => Cow::Borrowed(" "),
            2 => Cow::Borrowed("  "),
            3 => Cow::Borrowed("   "),
            _ => Cow::Owned(" ".repeat(pad_len)),
        };
        print_colored_text(stdout, &padding, Color::Black, None, None);
    }
}

/// Format CPU time in TIME+ format (e.g., `0:01:30`, `1:23:45`, `8760:00:00`).
///
/// For extremely long-running basic system processes (> 365 days), show as
/// `0:00:00` to avoid clutter.
///
/// # Width invariant
///
/// The output is at most **10 characters** long (`"8760:00:00"`, the 365-day
/// cap). The TIME+ column in `print_process_info` relies on this bound:
/// `fixed_widths[11] = 10`. If this function is changed to produce longer
/// strings, the column width must be bumped to match, otherwise overflowing
/// values push the Command column right and break alignment with the header.
fn format_cpu_time(seconds: u64) -> Cow<'static, str> {
    if seconds == 0 {
        return Cow::Borrowed("0:00:00");
    }

    // If the process has been running for more than 365 days (basic system process)
    // show as 0:00:00 to avoid clutter
    if seconds > 365 * 24 * 3600 {
        return Cow::Borrowed("0:00:00");
    }

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        Cow::Owned(format!("{hours}:{minutes:02}:{secs:02}"))
    } else {
        Cow::Owned(format!("{}:{:02}:{secs:02}", minutes / 60, minutes % 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // write_memory_size: correctness
    // -----------------------------------------------------------------------

    #[test]
    fn test_write_memory_size_zero() {
        let mut buf = String::new();
        write_memory_size(&mut buf, 0);
        assert_eq!(buf, "0");
    }

    #[test]
    fn test_write_memory_size_bytes() {
        let mut buf = String::new();
        write_memory_size(&mut buf, 512);
        assert_eq!(buf, "512");
    }

    #[test]
    fn test_write_memory_size_kilobytes() {
        let mut buf = String::new();
        write_memory_size(&mut buf, 4 * 1024);
        assert_eq!(buf, "4K");
    }

    #[test]
    fn test_write_memory_size_megabytes() {
        let mut buf = String::new();
        write_memory_size(&mut buf, 256 * 1024 * 1024);
        assert_eq!(buf, "256M");
    }

    #[test]
    fn test_write_memory_size_gigabytes() {
        let mut buf = String::new();
        write_memory_size(&mut buf, 8 * 1024 * 1024 * 1024);
        assert_eq!(buf, "8G");
    }

    #[test]
    fn test_write_memory_size_terabytes() {
        let mut buf = String::new();
        // 2TB = 2048 GB
        write_memory_size(&mut buf, 2 * 1024 * 1024 * 1024 * 1024);
        assert_eq!(buf, "2T");
    }

    #[test]
    fn test_write_memory_size_reuses_buffer() {
        // Verify the buffer is appended to, not replaced
        let mut buf = String::from("prefix-");
        write_memory_size(&mut buf, 1024 * 1024);
        assert_eq!(buf, "prefix-1M");
    }

    // -----------------------------------------------------------------------
    // format_cpu_time: correctness and Cow allocation behavior
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_cpu_time_zero_borrows() {
        let result = format_cpu_time(0);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "0:00:00");
    }

    #[test]
    fn test_format_cpu_time_long_running_borrows() {
        // More than 365 days should return the borrowed "0:00:00"
        let result = format_cpu_time(366 * 24 * 3600);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "0:00:00");
    }

    #[test]
    fn test_format_cpu_time_minutes_only() {
        // 90 seconds = 1 minute 30 seconds, no hours
        let result = format_cpu_time(90);
        assert!(matches!(result, Cow::Owned(_)));
        assert_eq!(&*result, "0:01:30");
    }

    #[test]
    fn test_format_cpu_time_with_hours() {
        // 3661 seconds = 1 hour, 1 minute, 1 second
        let result = format_cpu_time(3661);
        assert!(matches!(result, Cow::Owned(_)));
        assert_eq!(&*result, "1:01:01");
    }

    #[test]
    fn test_format_cpu_time_exact_one_hour() {
        let result = format_cpu_time(3600);
        assert_eq!(&*result, "1:00:00");
    }

    #[test]
    fn test_format_cpu_time_just_below_limit() {
        // 365 days exactly: should NOT be suppressed (must be > 365*24*3600)
        let at_limit = 365 * 24 * 3600;
        let result = format_cpu_time(at_limit);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_format_cpu_time_max_width_fits_time_column() {
        // Alignment invariant: the TIME+ column in `print_process_info` is
        // sized to the longest value that `format_cpu_time` can produce. If
        // this ever changes (e.g. the 365-day cap is lifted), the column
        // width must be bumped to match, otherwise the Command column will
        // drift right for overflowing rows and no longer align with the
        // header. See `fixed_widths[11]` in `print_process_info`.
        const TIME_COL_WIDTH: usize = 10;

        // The worst case under the current cap is 365 days = 8760 hours,
        // which renders as "8760:00:00" (10 chars).
        let at_limit = 365 * 24 * 3600;
        assert_eq!(&*format_cpu_time(at_limit), "8760:00:00");
        assert_eq!(format_cpu_time(at_limit).len(), TIME_COL_WIDTH);

        // A handful of realistic and boundary values must all fit in the
        // column so right-alignment produces a consistent Command position.
        for &secs in &[
            0u64,
            59,           // "0:00:59"
            90,           // "0:01:30"
            3599,         // "0:59:59"
            3600,         // "1:00:00"
            35_999,       // "9:59:59"
            36_000,       // "10:00:00"
            359_999,      // "99:59:59"
            360_000,      // "100:00:00"
            3_599_999,    // "999:59:59"
            3_600_000,    // "1000:00:00"
            at_limit - 1, // "8759:59:59"
            at_limit,     // "8760:00:00"
        ] {
            let rendered = format_cpu_time(secs);
            assert!(
                rendered.len() <= TIME_COL_WIDTH,
                "format_cpu_time({secs}) = {rendered:?} exceeds TIME_COL_WIDTH={TIME_COL_WIDTH}; \
                 this will break process list alignment. Widen `fixed_widths[11]` in \
                 `print_process_info` to match."
            );
        }
    }

    // -----------------------------------------------------------------------
    // RowFormatter: scratch buffer reuse
    // -----------------------------------------------------------------------

    #[test]
    fn test_row_formatter_clear_retains_capacity() {
        let mut rf = RowFormatter::new();
        rf.buf.push_str("some content longer than initial");
        let cap_before = rf.buf.capacity();
        rf.clear();
        assert!(rf.buf.is_empty());
        assert_eq!(rf.buf.capacity(), cap_before);
    }

    #[test]
    fn test_row_formatter_initial_capacity() {
        let rf = RowFormatter::new();
        assert!(rf.buf.capacity() >= 512);
    }

    // -----------------------------------------------------------------------
    // format_cpu_time throughput: hot-path allocation measurement
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_cpu_time_zero_throughput() {
        // The zero-seconds case must be zero-allocation (Cow::Borrowed).
        // Verify it completes quickly for the hot path.
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let r = format_cpu_time(0);
            assert!(matches!(r, Cow::Borrowed(_)));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "format_cpu_time(0) throughput too slow: {elapsed:?}"
        );
    }

    #[test]
    fn test_write_memory_size_throughput() {
        // 100k calls with mixed inputs should complete quickly
        let start = std::time::Instant::now();
        for i in 0..100_000u64 {
            let mut buf = String::with_capacity(8);
            write_memory_size(&mut buf, i * 1024);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "write_memory_size throughput too slow: {elapsed:?}"
        );
    }
}
