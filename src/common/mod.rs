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

/// One-shot migration of legacy cache paths, run by the binary at startup.
#[cfg(feature = "cli")]
#[doc(hidden)]
pub mod cache_migration;
/// Configuration management.
pub mod config;
/// File-level merge helpers for the TOML config (issue #192).
#[cfg(feature = "cli")]
pub mod config_apply;
/// Environment-variable overlay for the TOML config (issue #192).
#[cfg(feature = "cli")]
pub mod config_env;
/// TOML configuration file loader (issue #192).
#[cfg(feature = "cli")]
pub mod config_file;
/// On-disk schema types for the TOML configuration file.
#[cfg(feature = "cli")]
pub mod config_schema;
/// HTTP remote-host list normalization and syntax validation.
#[cfg(feature = "cli")]
pub mod http_hosts;
/// Platform-aware configuration path resolution.
#[cfg(feature = "cli")]
pub mod paths;
/// Shared secure file-write helper (O_NOFOLLOW + 0o600).
#[cfg(feature = "cli")]
pub mod secure_write;
#[cfg(test)]
pub(crate) mod test_env;
