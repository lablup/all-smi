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

use crossterm::style::Color;

/// Get utilization block character and color based on usage percentage
pub fn get_utilization_block(utilization: f64) -> (&'static str, Color) {
    match utilization {
        u if u >= 90.0 => ("█", Color::Red), // Full block, red for high usage
        u if u >= 80.0 => ("▇", Color::Magenta), // 7/8 block
        u if u >= 70.0 => ("▆", Color::Yellow), // 6/8 block
        u if u >= 60.0 => ("▅", Color::Yellow), // 5/8 block
        u if u >= 50.0 => ("▄", Color::Green), // 4/8 block
        u if u >= 40.0 => ("▃", Color::Green), // 3/8 block
        u if u >= 30.0 => ("▂", Color::Cyan), // 2/8 block
        u if u >= 20.0 => ("▁", Color::Cyan), // 1/8 block
        u if u >= 10.0 => ("▁", Color::Blue), // Low usage
        _ => ("▁", Color::DarkGrey),         // Minimal or no usage (still show lowest bar)
    }
}

/// Gauge style constants
pub const GAUGE_HIGH_COLOR: Color = Color::Red;
pub const GAUGE_MEDIUM_COLOR: Color = Color::Yellow;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_utilization_block() {
        // Test high utilization
        let (block, color) = get_utilization_block(95.0);
        assert_eq!(block, "█");
        assert_eq!(color, Color::Red);

        let (block, color) = get_utilization_block(85.0);
        assert_eq!(block, "▇");
        assert_eq!(color, Color::Magenta);

        // Test medium utilization
        let (block, color) = get_utilization_block(75.0);
        assert_eq!(block, "▆");
        assert_eq!(color, Color::Yellow);

        let (block, color) = get_utilization_block(55.0);
        assert_eq!(block, "▄");
        assert_eq!(color, Color::Green);

        // Test low utilization
        let (block, color) = get_utilization_block(35.0);
        assert_eq!(block, "▂");
        assert_eq!(color, Color::Cyan);

        let (block, color) = get_utilization_block(15.0);
        assert_eq!(block, "▁");
        assert_eq!(color, Color::Blue);

        // Test minimal utilization
        let (block, color) = get_utilization_block(5.0);
        assert_eq!(block, "▁");
        assert_eq!(color, Color::DarkGrey);

        let (block, color) = get_utilization_block(0.0);
        assert_eq!(block, "▁");
        assert_eq!(color, Color::DarkGrey);
    }
}
