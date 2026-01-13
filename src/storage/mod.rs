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

//! Storage monitoring module.
//!
//! This module provides storage/disk information reading capabilities
//! for local system monitoring.

pub mod info;
pub mod reader;

// Re-export commonly used items for the public library API.
// These exports are used by the prelude module and external library users,
// even though internal code may import from submodules directly.
#[allow(unused_imports)]
pub use info::StorageInfo;
#[allow(unused_imports)]
pub use reader::{create_storage_reader, LocalStorageReader, StorageReader};
