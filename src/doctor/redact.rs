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

//! Regex-based scrubbing of hostnames, IP addresses, MAC addresses, local
//! usernames, and kernel pointers. Applied by default to every bundle file
//! and to the top-level report unless `--include-identifiers` is set.

use std::sync::OnceLock;

use regex::Regex;

/// Replacement strings used in redacted output.
pub const REDACT_HOST: &str = "<hostname>";
pub const REDACT_IPV4: &str = "<ipv4>";
pub const REDACT_IPV6: &str = "<ipv6>";
pub const REDACT_MAC: &str = "<mac>";
pub const REDACT_USER: &str = "<user>";
pub const REDACT_KPTR: &str = "<kernel-ptr>";

fn ipv4_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Match dotted-quad with each octet 0-255. Anchored with word
        // boundaries so we don't eat fragments of version numbers.
        Regex::new(
            r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\b",
        )
        .expect("ipv4 regex must compile")
    })
}

fn ipv6_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Lenient IPv6 matcher covering the common shapes (full, :: compressed,
        // trailing ::1, embedded IPv4 of the form ::ffff:a.b.c.d). Avoids
        // consuming bare "::" in comments.
        Regex::new(
            r"\b(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}\b|\b(?:[0-9A-Fa-f]{1,4}:){1,7}:(?:[0-9A-Fa-f]{1,4})?\b|\b::(?:[0-9A-Fa-f]{1,4}:){0,6}[0-9A-Fa-f]{1,4}\b|\bfe80::[0-9A-Fa-f:]+\b",
        )
        .expect("ipv6 regex must compile")
    })
}

fn mac_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b").expect("mac regex must compile")
    })
}

fn kptr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches 16-char hex kernel addresses commonly emitted by dmesg
        // when kptr_restrict is on (e.g. "ffff8abc12345678"). Limited to
        // addresses with the `ffff` prefix so we don't scrub unrelated
        // hex sequences.
        Regex::new(r"\bffff[0-9a-f]{12}\b").expect("kptr regex must compile")
    })
}

fn username_token(name: &str) -> Regex {
    Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("username regex must compile")
}

fn hostname_token(name: &str) -> Regex {
    Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("hostname regex must compile")
}

/// Options controlling what [`scrub`] replaces.
#[derive(Clone, Debug)]
pub struct RedactOptions {
    /// Detected hostname; matched verbatim and replaced with [`REDACT_HOST`].
    pub hostname: Option<String>,
    /// Detected local username; matched verbatim and replaced with
    /// [`REDACT_USER`].
    pub username: Option<String>,
    /// When `true`, kernel addresses of the form `ffff...` (e.g. dmesg
    /// output on hosts with `kptr_restrict=0`) are scrubbed to
    /// [`REDACT_KPTR`]. The default is `true` so bundles are safe to
    /// upload even on permissive kernels.
    pub scrub_kernel_pointers: bool,
    /// Convenience knob: when `false`, [`scrub`] returns its input
    /// unchanged. Set to `false` under `--include-identifiers`.
    pub enabled: bool,
}

impl Default for RedactOptions {
    fn default() -> Self {
        Self {
            hostname: std::env::var("HOSTNAME")
                .ok()
                .or_else(hostname_from_libc)
                .filter(|h| !h.is_empty()),
            username: std::env::var("USER")
                .ok()
                .or_else(|| std::env::var("LOGNAME").ok())
                .filter(|u| !u.is_empty()),
            scrub_kernel_pointers: true,
            enabled: true,
        }
    }
}

impl RedactOptions {
    /// Build an options struct that performs no substitutions. Used when
    /// `--include-identifiers` is set.
    pub fn passthrough() -> Self {
        Self {
            hostname: None,
            username: None,
            scrub_kernel_pointers: false,
            enabled: false,
        }
    }
}

fn hostname_from_libc() -> Option<String> {
    // The `whoami` crate's `hostname()` already wraps the libc syscall and
    // returns a Result; map the error case to `None` so callers can fall
    // back to the `HOSTNAME` env var.
    whoami::hostname().ok().filter(|h| !h.is_empty())
}

/// Scrub identifiers out of `input` according to `opts`. When
/// `opts.enabled` is false, `input` is returned unchanged (cheap clone).
pub fn scrub(input: &str, opts: &RedactOptions) -> String {
    if !opts.enabled {
        return input.to_string();
    }

    let mut out = input.to_string();

    // Hostname and username substitutions first so the later regexes don't
    // partially collide with them (a hostname can contain digits that look
    // like fragments of an address).
    if let Some(ref host) = opts.hostname
        && !host.is_empty()
    {
        out = hostname_token(host)
            .replace_all(&out, REDACT_HOST)
            .into_owned();
    }
    if let Some(ref user) = opts.username
        && !user.is_empty()
    {
        out = username_token(user)
            .replace_all(&out, REDACT_USER)
            .into_owned();
    }

    out = ipv6_re().replace_all(&out, REDACT_IPV6).into_owned();
    out = ipv4_re().replace_all(&out, REDACT_IPV4).into_owned();
    out = mac_re().replace_all(&out, REDACT_MAC).into_owned();
    if opts.scrub_kernel_pointers {
        out = kptr_re().replace_all(&out, REDACT_KPTR).into_owned();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_ipv4_plain() {
        let opts = RedactOptions {
            hostname: None,
            username: None,
            ..RedactOptions::default()
        };
        let got = scrub("link 192.168.1.42 is up", &opts);
        assert!(got.contains(REDACT_IPV4));
        assert!(!got.contains("192.168.1.42"));
    }

    #[test]
    fn scrub_mac() {
        let opts = RedactOptions {
            hostname: None,
            username: None,
            ..RedactOptions::default()
        };
        let got = scrub("ether 02:42:ac:11:00:02 brd", &opts);
        assert!(got.contains(REDACT_MAC));
    }

    #[test]
    fn scrub_username() {
        let opts = RedactOptions {
            hostname: None,
            username: Some("alice".to_string()),
            ..RedactOptions::default()
        };
        let got = scrub("/home/alice/logs", &opts);
        assert!(got.contains(REDACT_USER));
        assert!(!got.contains("/alice/"));
    }

    #[test]
    fn scrub_kernel_pointer() {
        let opts = RedactOptions {
            hostname: None,
            username: None,
            ..RedactOptions::default()
        };
        let got = scrub("fault at ffff8abc12345678 (oops)", &opts);
        assert!(got.contains(REDACT_KPTR));
    }

    #[test]
    fn passthrough_skips_work() {
        let opts = RedactOptions::passthrough();
        let input = "host alice 10.0.0.1";
        assert_eq!(scrub(input, &opts), input);
    }
}
