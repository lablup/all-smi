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

//! `network.*` checks — only run when `--remote-check` is passed. For each
//! supplied host/URL we perform DNS resolution, TCP reachability, and a
//! best-effort HTTP GET on `/metrics`. All operations are bounded by the
//! per-check 3-second ceiling enforced by the orchestrator.

use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

use url::Url;

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&DNS, &TCP, &HTTP];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static DNS: Check = Check {
    id: "network.dns",
    title: "DNS resolution for --remote-check targets",
    severity_on_fail: Severity::Warn,
    run: check_dns,
};

static TCP: Check = Check {
    id: "network.tcp",
    title: "TCP reachability for --remote-check targets",
    severity_on_fail: Severity::Warn,
    run: check_tcp,
};

static HTTP: Check = Check {
    id: "network.http",
    title: "HTTP /metrics probe for --remote-check targets",
    severity_on_fail: Severity::Warn,
    run: check_http,
};

fn parse_target(raw: &str) -> Option<(String, u16, Option<Url>)> {
    if raw.contains("://") {
        let url = Url::parse(raw).ok()?;
        let host = url.host_str()?.to_string();
        let port = url.port_or_known_default()?;
        Some((host, port, Some(url)))
    } else if let Some((host, port_str)) = raw.rsplit_once(':') {
        let port: u16 = port_str.parse().ok()?;
        Some((host.to_string(), port, None))
    } else {
        Some((raw.to_string(), 9090, None))
    }
}

fn check_dns(ctx: &CheckCtx) -> CheckResult {
    if ctx.remote_checks.is_empty() {
        return CheckResult::Skip("no --remote-check targets".to_string());
    }
    let mut failures: Vec<String> = Vec::new();
    let mut successes: Vec<String> = Vec::new();
    for raw in &ctx.remote_checks {
        let Some((host, port, _)) = parse_target(raw) else {
            failures.push(format!("{raw}: unparseable"));
            continue;
        };
        match format!("{host}:{port}").to_socket_addrs() {
            Ok(iter) => {
                let count = iter.count();
                successes.push(format!("{raw}: {count} addr"));
            }
            Err(e) => failures.push(format!("{raw}: {e}")),
        }
    }
    if failures.is_empty() {
        CheckResult::Pass(successes.join("; "))
    } else {
        CheckResult::Fail(
            failures.join("; "),
            Some("verify the host name spelling and DNS resolver".to_string()),
        )
    }
}

fn check_tcp(ctx: &CheckCtx) -> CheckResult {
    if ctx.remote_checks.is_empty() {
        return CheckResult::Skip("no --remote-check targets".to_string());
    }

    let mut results: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // Per-target attempt bounded to 750 ms — combined with the number of
    // targets this is well inside the 3-second per-check ceiling.
    let per_target = Duration::from_millis(750);

    for raw in &ctx.remote_checks {
        let Some((host, port, _)) = parse_target(raw) else {
            failures.push(format!("{raw}: unparseable"));
            continue;
        };
        let addrs = match format!("{host}:{port}").to_socket_addrs() {
            Ok(v) => v.collect::<Vec<_>>(),
            Err(e) => {
                failures.push(format!("{raw}: resolve: {e}"));
                continue;
            }
        };
        let start = Instant::now();
        let mut ok = false;
        for a in addrs.iter() {
            if std::net::TcpStream::connect_timeout(a, per_target).is_ok() {
                ok = true;
                break;
            }
        }
        let elapsed = start.elapsed().as_millis();
        if ok {
            results.push(format!("{raw}: ok in {elapsed} ms"));
        } else {
            failures.push(format!("{raw}: unreachable"));
        }
    }

    if failures.is_empty() {
        CheckResult::Pass(results.join("; "))
    } else if results.is_empty() {
        CheckResult::Fail(
            failures.join("; "),
            Some("check firewall rules and the remote process status".to_string()),
        )
    } else {
        CheckResult::Warn(
            format!(
                "{} ok, {} failed: {}",
                results.len(),
                failures.len(),
                failures.join("; ")
            ),
            Some("check firewall rules for the failing endpoints".to_string()),
        )
    }
}

fn check_http(ctx: &CheckCtx) -> CheckResult {
    if ctx.remote_checks.is_empty() {
        return CheckResult::Skip("no --remote-check targets".to_string());
    }

    // Build a blocking-free request sequence using reqwest's blocking
    // shim through tokio::task::block_in_place isn't possible from a
    // spawn_blocking worker. Use the synchronous `ureq`-like flow via
    // tokio runtime — but since this entire function runs inside
    // spawn_blocking, we use a dedicated one-shot current-thread runtime
    // to drive the async reqwest call with a short deadline.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return CheckResult::Warn(format!("failed to spin up sub-runtime: {e}"), None);
        }
    };

    let mut results: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for raw in &ctx.remote_checks {
        let Some((host, port, url_opt)) = parse_target(raw) else {
            failures.push(format!("{raw}: unparseable"));
            continue;
        };
        let url = match url_opt {
            Some(u) => {
                let mut u = u;
                if u.path() == "/" || u.path().is_empty() {
                    u.set_path("/metrics");
                }
                u.to_string()
            }
            None => format!("http://{host}:{port}/metrics"),
        };
        let outcome = rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(1_500))
                .connect_timeout(Duration::from_millis(750))
                .build()
                .map_err(|e| e.to_string())?;
            let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
            Ok::<u16, String>(resp.status().as_u16())
        });
        match outcome {
            Ok(code) if (200..300).contains(&code) => {
                results.push(format!("{raw}: HTTP {code}"));
            }
            Ok(code) => {
                failures.push(format!("{raw}: HTTP {code}"));
            }
            Err(e) => failures.push(format!("{raw}: {e}")),
        }
    }

    if failures.is_empty() {
        CheckResult::Pass(results.join("; "))
    } else if results.is_empty() {
        CheckResult::Fail(
            failures.join("; "),
            Some("verify all-smi api is running on the remote and /metrics is exposed".to_string()),
        )
    } else {
        CheckResult::Warn(
            format!(
                "{} ok, {} failed: {}",
                results.len(),
                failures.len(),
                failures.join("; ")
            ),
            Some("verify all-smi api is running on the failing remote(s)".to_string()),
        )
    }
}
