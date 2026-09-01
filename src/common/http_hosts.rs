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

use thiserror::Error;
use url::Url;

/// A syntactically invalid HTTP remote endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid remote host `{value}`: {reason}")]
pub struct HttpHostError {
    value: String,
    reason: String,
}

impl HttpHostError {
    fn new(value: &str, reason: impl Into<String>) -> Self {
        Self {
            value: if value.is_empty() {
                "<empty>".to_string()
            } else {
                value.to_string()
            },
            reason: reason.into(),
        }
    }
}

/// Split a host list on commas, trim each element, and validate every endpoint.
///
/// Whitespace-separated CLI values already arrive as separate vector elements,
/// so applying comma splitting to each element supports whitespace, comma, and
/// mixed invocations while preserving the operator's ordering and explicit
/// scheme spelling.
pub fn normalize_http_hosts(hosts: &[String]) -> Result<Vec<String>, HttpHostError> {
    let mut normalized = Vec::new();

    for raw in hosts {
        for element in raw.split(',') {
            let host = element.trim();
            if host.is_empty() {
                return Err(HttpHostError::new(
                    raw.trim(),
                    "host list contains an empty entry",
                ));
            }
            parse_http_host_url(host)?;
            normalized.push(host.to_string());
        }
    }

    Ok(normalized)
}

/// Parse one endpoint without performing DNS lookup or any network I/O.
pub(crate) fn parse_http_host_url(host: &str) -> Result<Url, HttpHostError> {
    if let Some((prefix, _)) = host.split_once(':')
        && matches!(prefix.to_ascii_lowercase().as_str(), "http" | "https")
        && !host.contains("://")
    {
        return Err(HttpHostError::new(
            host,
            "malformed URL: HTTP and HTTPS schemes must be followed by `://`",
        ));
    }

    let candidate = if let Some((scheme, remainder)) = host.split_once("://") {
        if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
            return Err(HttpHostError::new(
                host,
                format!("unsupported scheme `{scheme}`; expected http or https"),
            ));
        }
        if remainder.is_empty() {
            return Err(HttpHostError::new(host, "missing host"));
        }
        host.to_string()
    } else {
        format!("http://{host}")
    };

    let url = Url::parse(&candidate)
        .map_err(|error| HttpHostError::new(host, format!("malformed URL: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(HttpHostError::new(
            host,
            format!(
                "unsupported scheme `{}`; expected http or https",
                url.scheme()
            ),
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(HttpHostError::new(host, "missing host"));
    }
    if url.port() == Some(0) {
        return Err(HttpHostError::new(host, "port must be between 1 and 65535"));
    }

    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn accepts_whitespace_comma_and_mixed_lists_in_order() {
        assert_eq!(
            normalize_http_hosts(&strings(&["host-a:9090", "host-b:9090"])).unwrap(),
            strings(&["host-a:9090", "host-b:9090"])
        );
        assert_eq!(
            normalize_http_hosts(&strings(&["host-a:9090,host-b:9090"])).unwrap(),
            strings(&["host-a:9090", "host-b:9090"])
        );
        assert_eq!(
            normalize_http_hosts(&strings(&[
                " host-a:9090, https://host-b:9443 ",
                "http://[2001:db8::1]:9090"
            ]))
            .unwrap(),
            strings(&[
                "host-a:9090",
                "https://host-b:9443",
                "http://[2001:db8::1]:9090"
            ])
        );
    }

    #[test]
    fn rejects_empty_elements() {
        for input in [
            "",
            ",host-a:9090",
            "host-a:9090,,host-b:9090",
            "host-a:9090,",
        ] {
            let error = normalize_http_hosts(&strings(&[input])).unwrap_err();
            assert!(error.to_string().contains("empty entry"), "{error}");
        }
    }

    #[test]
    fn rejects_invalid_port_and_names_the_value() {
        let error = normalize_http_hosts(&strings(&["host-a:9090,host-b:not-a-port"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("host-b:not-a-port"), "{error}");
        assert!(error.contains("invalid port"), "{error}");
    }

    #[test]
    fn rejects_unsupported_scheme_missing_host_and_malformed_url() {
        let unsupported = normalize_http_hosts(&strings(&["ftp://host-a:9090"]))
            .unwrap_err()
            .to_string();
        assert!(unsupported.contains("ftp://host-a:9090"), "{unsupported}");
        assert!(unsupported.contains("unsupported scheme"), "{unsupported}");

        let missing = normalize_http_hosts(&strings(&["http://"]))
            .unwrap_err()
            .to_string();
        assert!(missing.contains("http://"), "{missing}");
        assert!(missing.contains("missing host"), "{missing}");

        let malformed = normalize_http_hosts(&strings(&["http://[2001:db8::1"]))
            .unwrap_err()
            .to_string();
        assert!(malformed.contains("http://[2001:db8::1"), "{malformed}");
        assert!(malformed.contains("malformed URL"), "{malformed}");

        let missing_slashes = normalize_http_hosts(&strings(&["https:/host-a:9090"]))
            .unwrap_err()
            .to_string();
        assert!(
            missing_slashes.contains("https:/host-a:9090"),
            "{missing_slashes}"
        );
        assert!(
            missing_slashes.contains("must be followed by `://`"),
            "{missing_slashes}"
        );
    }

    #[test]
    fn preserves_https_and_accepts_unreachable_or_bracketed_ipv6_hosts() {
        let hosts = normalize_http_hosts(&strings(&[
            "https://does-not-exist.invalid:9443",
            "[2001:db8::2]:9090",
        ]))
        .unwrap();
        assert_eq!(
            hosts,
            strings(&["https://does-not-exist.invalid:9443", "[2001:db8::2]:9090"])
        );
        assert_eq!(parse_http_host_url(&hosts[0]).unwrap().scheme(), "https");
    }
}
