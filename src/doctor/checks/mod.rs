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

//! Check registry for the `doctor` subcommand.
//!
//! Each submodule exposes a `checks()` function returning a slice of
//! [`Check`] descriptors. The top-level [`all`] function concatenates
//! them into a single vector that the orchestrator filters and drives.

use crate::doctor::types::Check;

pub mod amd;
pub mod apple;
pub mod container;
pub mod env;
pub mod furiosa;
pub mod gaudi;
pub mod network;
pub mod nvidia;
pub mod platform;
pub mod privileges;
pub mod rebellions;
pub mod tenstorrent;
pub mod tpu;
pub mod windows;

/// Collect every built-in check. Order is not guaranteed to be stable;
/// the orchestrator sorts outcomes by check ID before rendering.
pub fn all() -> Vec<&'static Check> {
    let mut v: Vec<&'static Check> = Vec::new();
    v.extend(platform::checks());
    v.extend(privileges::checks());
    v.extend(container::checks());
    v.extend(nvidia::checks());
    v.extend(amd::checks());
    v.extend(apple::checks());
    v.extend(gaudi::checks());
    v.extend(tpu::checks());
    v.extend(tenstorrent::checks());
    v.extend(rebellions::checks());
    v.extend(furiosa::checks());
    v.extend(windows::checks());
    v.extend(env::checks());
    v.extend(network::checks());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn check_ids_are_unique() {
        // Duplicate IDs would confuse --skip / --only and breaks the
        // "grep output for stable IDs" contract in the issue.
        let ids: Vec<&str> = all().iter().map(|c| c.id).collect();
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "duplicate check IDs: {ids:?}");
    }

    #[test]
    fn check_ids_follow_dotted_convention() {
        for c in all() {
            assert!(
                c.id.contains('.'),
                "check id {:?} must contain a dot separator",
                c.id
            );
        }
    }
}
