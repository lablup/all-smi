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

//! Transport chip + connection-state types shared between the SSH
//! strategy and the TUI renderer (issue #194).

use std::fmt;

/// Which command a given SSH host is actually reachable through.
///
/// The SSH strategy probes each host once on first connect and caches
/// the result for the lifetime of the view session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SshTransport {
    /// Host has `all-smi` installed and we run the native `snapshot`
    /// subcommand. Full fidelity (CPU, memory, chassis, GPU).
    Native,
    /// Fallback: `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`.
    /// GPU only — CPU and chassis remain empty.
    NvidiaSmi,
    /// Fallback: `rocm-smi ... --json`. GPU only.
    RocmSmi,
    /// Host responded to SSH but none of the configured probes worked.
    /// UI surfaces a dim "unsupported" chip.
    #[default]
    Unsupported,
}

impl SshTransport {
    /// Short label used as the TUI transport chip (issue #194 UI spec).
    pub fn chip_label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::NvidiaSmi => "nvidia-smi",
            Self::RocmSmi => "rocm-smi",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for SshTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.chip_label())
    }
}

/// Which fallback probes the operator opted into. Parsed from
/// `--ssh-fallback`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshFallbackPolicy {
    pub try_nvidia_smi: bool,
    pub try_rocm_smi: bool,
}

impl SshFallbackPolicy {
    /// Build a policy from the comma-separated CLI string.
    ///
    /// * Empty or missing → default: both NVIDIA and ROCm fallbacks
    ///   enabled. This is the least-surprise behaviour: operators who
    ///   do not care about the flag still get fallback coverage on
    ///   hosts that have neither `all-smi` nor the matching SMI tool.
    /// * `none` → disable all fallbacks. Hosts without `all-smi` render
    ///   as unsupported.
    /// * Any of `nvidia-smi`, `rocm-smi` → enable only the named probes.
    pub fn from_cli(raw: Option<&str>) -> Result<Self, SshFallbackPolicyError> {
        let Some(s) = raw else {
            return Ok(Self::default_enabled());
        };
        let s = s.trim();
        if s.is_empty() {
            return Ok(Self::default_enabled());
        }

        let mut policy = Self::default();
        let mut saw_explicit_none = false;
        for token in s.split(',') {
            let token = token.trim().to_ascii_lowercase();
            match token.as_str() {
                "" => continue,
                "none" | "off" | "disabled" => {
                    saw_explicit_none = true;
                }
                "nvidia-smi" | "nvidia_smi" | "nvidia" => policy.try_nvidia_smi = true,
                "rocm-smi" | "rocm_smi" | "rocm" | "amd" => policy.try_rocm_smi = true,
                other => {
                    return Err(SshFallbackPolicyError::Unknown(other.to_string()));
                }
            }
        }

        if saw_explicit_none {
            // `--ssh-fallback none` should win over any accompanying
            // shim name: the operator explicitly asked for no fallbacks.
            return Ok(Self::default());
        }
        Ok(policy)
    }

    fn default_enabled() -> Self {
        Self {
            try_nvidia_smi: true,
            try_rocm_smi: true,
        }
    }

    /// True if at least one fallback shim is enabled.
    #[allow(dead_code)]
    pub fn any_enabled(&self) -> bool {
        self.try_nvidia_smi || self.try_rocm_smi
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SshFallbackPolicyError {
    #[error("unknown fallback kind `{0}` (valid: nvidia-smi, rocm-smi, none)")]
    Unknown(String),
}

/// Host-key policy from `--ssh-strict-host-key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrictHostKey {
    /// Default. Refuse unknown hosts.
    #[default]
    Yes,
    /// Accept unknown hosts on first connect; reject when a known key
    /// changes.
    AcceptNew,
    /// Accept any key (emits a prominent TUI warning).
    No,
}

impl StrictHostKey {
    pub fn from_cli(raw: &str) -> Result<Self, StrictHostKeyError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" | "strict" => Ok(Self::Yes),
            "accept-new" | "acceptnew" | "accept_new" => Ok(Self::AcceptNew),
            "no" | "false" | "off" => Ok(Self::No),
            other => Err(StrictHostKeyError::Unknown(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StrictHostKeyError {
    #[error("unknown --ssh-strict-host-key value `{0}` (valid: yes, accept-new, no)")]
    Unknown(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_chip_label_is_stable() {
        assert_eq!(SshTransport::Native.chip_label(), "native");
        assert_eq!(SshTransport::NvidiaSmi.chip_label(), "nvidia-smi");
        assert_eq!(SshTransport::RocmSmi.chip_label(), "rocm-smi");
        assert_eq!(SshTransport::Unsupported.chip_label(), "unsupported");
    }

    #[test]
    fn policy_default_enables_both_shims() {
        let p = SshFallbackPolicy::from_cli(None).unwrap();
        assert!(p.try_nvidia_smi);
        assert!(p.try_rocm_smi);
    }

    #[test]
    fn policy_none_disables_both() {
        let p = SshFallbackPolicy::from_cli(Some("none")).unwrap();
        assert!(!p.try_nvidia_smi);
        assert!(!p.try_rocm_smi);
        assert!(!p.any_enabled());
    }

    #[test]
    fn policy_only_nvidia() {
        let p = SshFallbackPolicy::from_cli(Some("nvidia-smi")).unwrap();
        assert!(p.try_nvidia_smi);
        assert!(!p.try_rocm_smi);
    }

    #[test]
    fn policy_both_explicitly() {
        let p = SshFallbackPolicy::from_cli(Some("nvidia-smi,rocm-smi")).unwrap();
        assert!(p.try_nvidia_smi);
        assert!(p.try_rocm_smi);
    }

    #[test]
    fn policy_none_wins_over_other_tokens() {
        // `--ssh-fallback nvidia-smi,none` should disable all probes.
        let p = SshFallbackPolicy::from_cli(Some("nvidia-smi,none")).unwrap();
        assert!(!p.any_enabled());
    }

    #[test]
    fn policy_rejects_unknown_token() {
        let e = SshFallbackPolicy::from_cli(Some("gpumonitor")).unwrap_err();
        matches!(e, SshFallbackPolicyError::Unknown(_));
    }

    #[test]
    fn strict_host_key_parses_all_variants() {
        assert_eq!(StrictHostKey::from_cli("yes").unwrap(), StrictHostKey::Yes);
        assert_eq!(
            StrictHostKey::from_cli("accept-new").unwrap(),
            StrictHostKey::AcceptNew
        );
        assert_eq!(StrictHostKey::from_cli("no").unwrap(), StrictHostKey::No);
    }

    #[test]
    fn strict_host_key_rejects_unknown() {
        let e = StrictHostKey::from_cli("sometimes").unwrap_err();
        matches!(e, StrictHostKeyError::Unknown(_));
    }
}
