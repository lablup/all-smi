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

//! Sysfs walking for Intel GPU engine-busy counter files.
//!
//! Split out of the parent module so the delta-computation core
//! (`refresh`, `EngineState`, lock handling) stays small and the
//! per-driver path-probing logic can grow independently.
//!
//! Path layouts handled (all relative to `device_dir`, i.e.
//! `/sys/class/drm/cardN/device`):
//!
//! - i915 flat:   `../engine/<class+instance>/busy`
//!   (e.g. `../engine/rcs0/busy`)
//! - i915 nested: `../engine/<class>/<instance>/busy`
//! - xe flat:     `tile<T>/gt<G>/engines/<class+instance>/busy_ns`
//! - xe nested:   `tile<T>/gt<G>/engines/<class>/<instance>/busy_ns`
//!
//! All counter files report a monotonic u64 busy duration in
//! nanoseconds. Class names are normalised to a small `'static str`
//! set by [`normalize_engine_class`].

use super::EngineCounter;
use std::path::Path;

/// Map a raw engine-class token (e.g. `rcs`, `RCS`, `RENDER`, `bcs`, …)
/// to a canonical short lowercase name used in `detail` map keys.
///
/// Mapping table:
/// - `rcs` / `render`           -> `"render"`
/// - `ccs` / `compute`          -> `"compute"`
/// - `bcs` / `copy`             -> `"copy"`
/// - `vcs` / `video` / `video_decode` -> `"video"`
/// - `vecs` / `video_enhance`   -> `"video-enhance"`
/// - anything else              -> `"other"`
pub fn normalize_engine_class(raw: &str) -> &'static str {
    let lowered = raw.to_ascii_lowercase();
    match lowered.as_str() {
        "rcs" | "render" => "render",
        "ccs" | "compute" => "compute",
        "bcs" | "copy" => "copy",
        "vcs" | "video" | "video_decode" | "video-decode" => "video",
        "vecs" | "video_enhance" | "video-enhance" => "video-enhance",
        _ => "other",
    }
}

/// Discover every engine-busy counter file for one card.
///
/// `device_dir` is the absolute path of `cardN/device/`. Returns the
/// counters sorted by class then instance so the per-engine `detail`
/// map keys do not flap from one refresh to the next. An empty Vec
/// means the kernel does not expose engine counters on this build.
pub fn discover_engine_counters(device_dir: &Path) -> Vec<EngineCounter> {
    let mut out = Vec::new();

    // i915 root is `cardN/engine/...` — parent of `cardN/device/`.
    // Also probe the device-rooted variant for forward-compatibility.
    if let Some(card_dir) = device_dir.parent() {
        collect_i915_counters(&card_dir.join("engine"), &mut out);
    }
    collect_i915_counters(&device_dir.join("engine"), &mut out);

    // xe: walk tileN/gtM trees. Two tiles x two GTs cover every
    // consumer xe SKU; multi-tile datacenter parts are out of scope
    // for the client GPU reader (#244).
    for tile in 0u32..2 {
        for gt in 0u32..2 {
            let engines_dir = device_dir
                .join(format!("tile{tile}"))
                .join(format!("gt{gt}"))
                .join("engines");
            collect_xe_counters(&engines_dir, &mut out);
        }
    }

    out.sort_by(|a, b| {
        a.class
            .cmp(b.class)
            .then_with(|| a.instance.cmp(&b.instance))
    });
    out
}

/// Walk an `engine/` directory laid out the i915 way. Handles both the
/// flat (`engine/rcs0/busy`) and nested (`engine/rcs/0/busy`) variants.
fn collect_i915_counters(engine_root: &Path, out: &mut Vec<EngineCounter>) {
    let entries = match std::fs::read_dir(engine_root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };

        // Flat layout: `engine/<class+instance>/busy`.
        let busy = path.join("busy");
        if busy.is_file() {
            let (class_raw, instance) = split_class_instance(&name);
            out.push(EngineCounter {
                class: normalize_engine_class(class_raw),
                instance,
                path: busy,
            });
            continue;
        }

        // Nested layout: `engine/<class>/<instance>/busy`.
        if let Ok(inner) = std::fs::read_dir(&path) {
            for inst_entry in inner.flatten() {
                let inst_path = inst_entry.path();
                let Some(inst_name) = inst_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                else {
                    continue;
                };
                let inst_busy = inst_path.join("busy");
                if inst_busy.is_file() {
                    out.push(EngineCounter {
                        class: normalize_engine_class(&name),
                        instance: inst_name,
                        path: inst_busy,
                    });
                }
            }
        }
    }
}

/// Walk an `engines/` directory laid out the xe way. Counter files are
/// named `busy_ns` but we accept `busy` too for forward-compatibility.
fn collect_xe_counters(engines_root: &Path, out: &mut Vec<EngineCounter>) {
    let entries = match std::fs::read_dir(engines_root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };

        // Flat xe form: `engines/<class+instance>/busy_ns`.
        let mut matched_flat = false;
        for filename in ["busy_ns", "busy"] {
            let candidate = path.join(filename);
            if candidate.is_file() {
                let (class_raw, instance) = split_class_instance(&name);
                out.push(EngineCounter {
                    class: normalize_engine_class(class_raw),
                    instance,
                    path: candidate,
                });
                matched_flat = true;
                break;
            }
        }
        if matched_flat {
            continue;
        }

        // Nested xe form: `engines/<class>/<instance>/busy_ns`.
        if let Ok(inner) = std::fs::read_dir(&path) {
            for inst_entry in inner.flatten() {
                let inst_path = inst_entry.path();
                let Some(inst_name) = inst_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                else {
                    continue;
                };
                for filename in ["busy_ns", "busy"] {
                    let inst_busy = inst_path.join(filename);
                    if inst_busy.is_file() {
                        out.push(EngineCounter {
                            class: normalize_engine_class(&name),
                            instance: inst_name.clone(),
                            path: inst_busy,
                        });
                        break;
                    }
                }
            }
        }
    }
}

/// Split a flat directory name (e.g. `rcs0`) into a class token and an
/// instance suffix. If the name has no trailing digits the whole string
/// is the class and the instance is empty.
pub(super) fn split_class_instance(name: &str) -> (&str, String) {
    let mut split_idx: Option<usize> = None;
    for (i, c) in name.char_indices().rev() {
        if c.is_ascii_digit() {
            split_idx = Some(i);
        } else {
            break;
        }
    }
    match split_idx {
        Some(i) if i > 0 => (&name[..i], name[i..].to_string()),
        // All-digits name: treat the whole thing as the class.
        Some(_) => (name, String::new()),
        None => (name, String::new()),
    }
}
