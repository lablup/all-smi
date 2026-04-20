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

//! Energy panel (issue #191).
//!
//! Renders the top energy consumers by device along with elapsed
//! session time and an average-power estimate.  Zero-energy devices
//! are suppressed so Apple Silicon / AMD hosts that do not expose
//! per-chassis power do not pollute the table.

use std::io::Write;
use std::time::Duration;

use crossterm::{queue, style::Color, style::Print};

use crate::common::config::EnergyConfig;
use crate::metrics::energy::{EnergyScope, PowerIntegrator, joules_to_cost, joules_to_kwh};
use crate::ui::text::print_colored_text;

/// Build a textual summary of the top energy consumers.
///
/// Returned as a `Vec<String>` so it can be rendered into either the
/// differential-rendering buffer used by the live TUI or a plain
/// `Vec<u8>` in tests.
///
/// `top_n` caps the number of per-device rows; the issue spec asks for
/// 3 but the caller may raise it for a debugging overlay.
#[allow(dead_code)] // Reserved for the optional `E` energy panel (issue #191).
pub fn format_top_consumers(
    integrator: &PowerIntegrator,
    energy_config: &EnergyConfig,
    top_n: usize,
) -> Vec<String> {
    let mut consumers: Vec<_> = integrator
        .iter_stats()
        .filter(|s| s.session_joules > 0.0)
        .collect();

    // Sort by session joules, descending. The tie-break on the key
    // guarantees deterministic ordering across scrapes so the top-N
    // list does not flicker when two devices are at the same draw.
    consumers.sort_by(|a, b| {
        b.session_joules
            .partial_cmp(&a.session_joules)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.host.cmp(&b.key.host))
            .then_with(|| a.key.device.cmp(&b.key.device))
    });

    let mut lines = Vec::new();
    if consumers.is_empty() {
        return lines;
    }

    lines.push("Top consumers (session):".to_string());
    for (idx, stat) in consumers.iter().take(top_n).enumerate() {
        let kwh = joules_to_kwh(stat.session_joules);
        let cost = if energy_config.cost_visible() {
            joules_to_cost(stat.session_joules, energy_config.price_per_kwh)
        } else {
            0.0
        };
        let scope_tag = match stat.key.scope {
            EnergyScope::Gpu => "gpu",
            EnergyScope::Cpu => "cpu",
            EnergyScope::Chassis => "chassis",
        };
        let label = match stat.key.scope {
            EnergyScope::Gpu => format!("{}/{}", stat.key.host, stat.key.device),
            EnergyScope::Cpu | EnergyScope::Chassis => stat.key.host.clone(),
        };
        let rank = idx + 1;
        let currency = &energy_config.currency;
        let line = if energy_config.cost_visible() {
            format!("  {rank}. {scope_tag:<8} {label:<40} {kwh:>8.3} kWh  {cost:>8.2} {currency}")
        } else {
            format!("  {rank}. {scope_tag:<8} {label:<40} {kwh:>8.3} kWh")
        };
        lines.push(line);
    }
    lines
}

/// Render the panel directly to a writer.
///
/// Used by the full-screen `E` section. Each line is padded with a
/// CRLF so the crossterm differential renderer sees the expected row
/// boundaries.
#[allow(dead_code)] // Reserved for the optional `E` energy panel (issue #191).
pub fn render_top_consumers<W: Write>(
    stdout: &mut W,
    integrator: &PowerIntegrator,
    energy_config: &EnergyConfig,
    top_n: usize,
    session_elapsed: Duration,
) {
    for line in format_top_consumers(integrator, energy_config, top_n) {
        print_colored_text(stdout, &line, Color::White, None, None);
        queue!(stdout, Print("\r\n")).unwrap();
    }

    // Cumulative chassis total across every host in the integrator,
    // plus the elapsed session time and the average-power estimate.
    let total_chassis_joules: f64 = integrator
        .iter_stats()
        .filter(|s| s.key.scope == EnergyScope::Chassis)
        .map(|s| s.session_joules)
        .sum();
    if total_chassis_joules <= 0.0 {
        return;
    }
    let elapsed_secs = session_elapsed.as_secs_f64().max(1.0);
    let avg_power = total_chassis_joules / elapsed_secs;
    let total_kwh = joules_to_kwh(total_chassis_joules);
    let elapsed_fmt = format_duration(session_elapsed);
    let summary = if energy_config.cost_visible() {
        let cost = joules_to_cost(total_chassis_joules, energy_config.price_per_kwh);
        format!(
            "  Total: {total_kwh:.3} kWh over {elapsed_fmt}  avg {avg_power:.1} W  cost {cost:.2} {currency}",
            currency = energy_config.currency,
        )
    } else {
        format!("  Total: {total_kwh:.3} kWh over {elapsed_fmt}  avg {avg_power:.1} W",)
    };
    print_colored_text(stdout, &summary, Color::Green, None, None);
    queue!(stdout, Print("\r\n")).unwrap();
}

/// Format a `Duration` as `HH:MM:SS` with no leading unit labels.
#[allow(dead_code)] // Reserved helper for the optional `E` energy panel.
fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::energy::{EnergyKey, PowerIntegrator};
    use std::time::{Duration, Instant};

    #[test]
    fn format_top_consumers_is_empty_when_no_energy() {
        let integrator = PowerIntegrator::default();
        let cfg = EnergyConfig::default();
        let out = format_top_consumers(&integrator, &cfg, 3);
        assert!(out.is_empty());
    }

    #[test]
    fn format_top_consumers_orders_by_session_joules_descending() {
        let mut integrator = PowerIntegrator::default();
        let origin = Instant::now();
        integrator.record_sample(EnergyKey::gpu("host-a", "uuid-0"), origin, 100.0);
        integrator.record_sample(
            EnergyKey::gpu("host-a", "uuid-0"),
            origin + Duration::from_secs(10),
            100.0,
        );
        integrator.record_sample(EnergyKey::gpu("host-a", "uuid-1"), origin, 300.0);
        integrator.record_sample(
            EnergyKey::gpu("host-a", "uuid-1"),
            origin + Duration::from_secs(10),
            300.0,
        );
        let cfg = EnergyConfig::default();
        let lines = format_top_consumers(&integrator, &cfg, 3);
        assert!(lines[0].contains("Top consumers"));
        let uuid1_line = lines
            .iter()
            .find(|l| l.contains("uuid-1"))
            .expect("uuid-1 line missing");
        let uuid0_line = lines
            .iter()
            .find(|l| l.contains("uuid-0"))
            .expect("uuid-0 line missing");
        let uuid1_pos = lines.iter().position(|l| l == uuid1_line).unwrap();
        let uuid0_pos = lines.iter().position(|l| l == uuid0_line).unwrap();
        assert!(
            uuid1_pos < uuid0_pos,
            "uuid-1 (300 W) should rank above uuid-0 (100 W):\n{lines:#?}"
        );
    }

    #[test]
    fn format_duration_produces_hh_mm_ss() {
        assert_eq!(format_duration(Duration::from_secs(0)), "00:00:00");
        assert_eq!(format_duration(Duration::from_secs(59)), "00:00:59");
        assert_eq!(format_duration(Duration::from_secs(3661)), "01:01:01");
    }

    #[test]
    fn render_top_consumers_prints_total_line_when_chassis_samples_exist() {
        let mut integrator = PowerIntegrator::default();
        let origin = Instant::now();
        integrator.record_sample(EnergyKey::chassis("host-a"), origin, 500.0);
        integrator.record_sample(
            EnergyKey::chassis("host-a"),
            origin + Duration::from_secs(60),
            500.0,
        );
        let cfg = EnergyConfig::default();
        let mut buffer = Vec::new();
        render_top_consumers(&mut buffer, &integrator, &cfg, 3, Duration::from_secs(60));
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("Total:"));
        assert!(output.contains("avg"));
    }
}
