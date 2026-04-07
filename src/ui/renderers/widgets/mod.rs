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

pub mod gauges;
pub mod tables;

// Re-export commonly used items
#[allow(unused_imports)]
pub use gauges::get_utilization_block;

// Re-export for future use
#[allow(unused_imports)]
pub use gauges::render_gauge;
#[allow(unused_imports)]
pub use tables::{TableRow, close_bordered_box, render_bordered_box, render_info_table};
