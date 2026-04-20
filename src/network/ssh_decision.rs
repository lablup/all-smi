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

//! Pure decision logic for SSH transport selection (issue #194).
//!
//! The actual SSH client lives in [`crate::network::ssh_client`]; this
//! module intentionally holds only the branchy selection code so it
//! can be unit-tested without mocking a full SSH server.

use crate::network::ssh_transport::{SshFallbackPolicy, SshTransport};

/// Minimum `all-smi` version that ships `snapshot --format json`.
/// Used by [`native_supported`] to gate the native path.
///
/// Matches issue #194 spec. Kept as a tuple for easy comparison.
pub const MIN_NATIVE_VERSION: (u32, u32) = (0, 22);

/// Outcome of probing one host during the initial SSH connection.
///
/// Each field is a `ProbeResult` so the decision function can reason
/// about three states per probe: ran successfully, ran but gave no
/// useful output, or was not attempted at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeOutcomes {
    pub native: ProbeResult,
    pub nvidia_smi: ProbeResult,
    pub rocm_smi: ProbeResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeResult {
    /// Probe was not attempted (e.g. native probe succeeded so we never
    /// reached the fallback, or the operator disabled it via policy).
    #[default]
    NotAttempted,
    /// Probe ran and reported "this is not installed / not useful".
    NotAvailable,
    /// Probe ran and returned a usable signature (for native: the
    /// `--version` line parsed to something ≥ MIN_NATIVE_VERSION; for
    /// fallbacks: the command exited 0 with non-empty stdout).
    Available,
}

/// Compute the transport chip to use from a set of probe outcomes.
///
/// Precedence (issue #194 spec):
/// 1. native (`all-smi` installed, new enough)
/// 2. nvidia-smi shim (if policy allows)
/// 3. rocm-smi shim (if policy allows)
/// 4. Unsupported
pub fn select_transport(outcomes: &ProbeOutcomes, policy: &SshFallbackPolicy) -> SshTransport {
    if outcomes.native == ProbeResult::Available {
        return SshTransport::Native;
    }
    if policy.try_nvidia_smi && outcomes.nvidia_smi == ProbeResult::Available {
        return SshTransport::NvidiaSmi;
    }
    if policy.try_rocm_smi && outcomes.rocm_smi == ProbeResult::Available {
        return SshTransport::RocmSmi;
    }
    SshTransport::Unsupported
}

/// Returns `true` when `version_line` (captured from `all-smi --version`
/// over SSH) satisfies [`MIN_NATIVE_VERSION`]. Anything that doesn't
/// parse cleanly returns `false` — we err on the side of using the
/// fallback shim over a potentially-unsafe native path.
pub fn native_supported(version_line: &str) -> bool {
    let Some(v) = extract_version(version_line) else {
        return false;
    };
    v >= MIN_NATIVE_VERSION
}

/// Extract a `(major, minor)` tuple from an `all-smi --version` line.
///
/// The binary prints `all-smi <version>`; we read the whitespace-
/// separated last token and split on `.`.
fn extract_version(line: &str) -> Option<(u32, u32)> {
    let token = line
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_start_matches('v');
    let mut parts = token.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts
        .next()
        .map(|s| s.split(['-', '+']).next().unwrap_or(s))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_all() -> SshFallbackPolicy {
        SshFallbackPolicy {
            try_nvidia_smi: true,
            try_rocm_smi: true,
        }
    }

    fn policy_none() -> SshFallbackPolicy {
        SshFallbackPolicy::default()
    }

    #[test]
    fn selects_native_when_available() {
        let outcomes = ProbeOutcomes {
            native: ProbeResult::Available,
            nvidia_smi: ProbeResult::Available, // Must be ignored
            rocm_smi: ProbeResult::NotAttempted,
        };
        assert_eq!(
            select_transport(&outcomes, &policy_all()),
            SshTransport::Native
        );
    }

    #[test]
    fn falls_back_to_nvidia_when_native_absent() {
        let outcomes = ProbeOutcomes {
            native: ProbeResult::NotAvailable,
            nvidia_smi: ProbeResult::Available,
            rocm_smi: ProbeResult::NotAttempted,
        };
        assert_eq!(
            select_transport(&outcomes, &policy_all()),
            SshTransport::NvidiaSmi
        );
    }

    #[test]
    fn falls_back_to_rocm_when_nvidia_unavailable() {
        let outcomes = ProbeOutcomes {
            native: ProbeResult::NotAvailable,
            nvidia_smi: ProbeResult::NotAvailable,
            rocm_smi: ProbeResult::Available,
        };
        assert_eq!(
            select_transport(&outcomes, &policy_all()),
            SshTransport::RocmSmi
        );
    }

    #[test]
    fn unsupported_when_no_probes_work() {
        let outcomes = ProbeOutcomes {
            native: ProbeResult::NotAvailable,
            nvidia_smi: ProbeResult::NotAvailable,
            rocm_smi: ProbeResult::NotAvailable,
        };
        assert_eq!(
            select_transport(&outcomes, &policy_all()),
            SshTransport::Unsupported
        );
    }

    #[test]
    fn policy_none_skips_fallbacks_even_when_available() {
        let outcomes = ProbeOutcomes {
            native: ProbeResult::NotAvailable,
            nvidia_smi: ProbeResult::Available,
            rocm_smi: ProbeResult::Available,
        };
        assert_eq!(
            select_transport(&outcomes, &policy_none()),
            SshTransport::Unsupported
        );
    }

    #[test]
    fn policy_only_rocm_ignores_nvidia() {
        let policy = SshFallbackPolicy {
            try_nvidia_smi: false,
            try_rocm_smi: true,
        };
        let outcomes = ProbeOutcomes {
            native: ProbeResult::NotAvailable,
            nvidia_smi: ProbeResult::Available,
            rocm_smi: ProbeResult::Available,
        };
        assert_eq!(select_transport(&outcomes, &policy), SshTransport::RocmSmi);
    }

    #[test]
    fn native_supported_accepts_exact_min_version() {
        assert!(native_supported("all-smi 0.22.0"));
        assert!(native_supported("all-smi 0.22.1"));
    }

    #[test]
    fn native_supported_rejects_older_versions() {
        assert!(!native_supported("all-smi 0.21.5"));
        assert!(!native_supported("all-smi 0.20.1"));
    }

    #[test]
    fn native_supported_accepts_newer_major() {
        assert!(native_supported("all-smi 1.0.0"));
        assert!(native_supported("all-smi 0.23.0"));
    }

    #[test]
    fn native_supported_handles_v_prefix() {
        assert!(native_supported("all-smi v0.22.0"));
    }

    #[test]
    fn native_supported_rejects_garbage() {
        assert!(!native_supported(""));
        assert!(!native_supported("not a version"));
        assert!(!native_supported("all-smi unknown"));
    }

    #[test]
    fn native_supported_handles_prerelease_suffix() {
        // `0.22.0-alpha.1` should count as 0.22 for gating purposes.
        assert!(native_supported("all-smi 0.22.0-alpha.1"));
    }
}
