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

//! Cross-file contract: every asset the crate embeds with `include_str!`
//! or `include_bytes!` must be reachable inside the Docker build context.
//!
//! `include_str!` resolves at compile time against the source tree, so a
//! new embedded asset compiles fine on a developer machine and in the
//! normal CI jobs, which all build from a full checkout. The Docker image
//! builds from a narrowed context assembled by explicit `COPY` lines in
//! the Dockerfile, so an asset outside that set fails the image build and
//! nothing else.
//!
//! Issue #309 added `packaging/systemd/all-smi.service` as the first such
//! asset. Every PR check passed and `main` went red on the merge commit
//! with `couldn't read src/service_cmd/../../packaging/systemd/all-smi.service`,
//! because `docker-check` was then gated on pushes to main and never ran
//! on a pull request.
//!
//! Since #328 that gate is gone and `docker-check` builds the image on
//! pull requests too, so a real `docker build` now backs every PR. These
//! tests are still the first line of defence rather than a leftover:
//! they run in the `test` job, `docker-check` is `needs: test`, and they
//! finish in milliseconds. For the classes they cover, the 5-6 minute
//! image build never starts, and the failure message names the offending
//! asset and the `COPY` line to add instead of surfacing as a raw
//! BuildKit error partway through a compile.
//!
//! Two directions are checked, and they are not the same check:
//!
//! - every embedded asset is reachable from some builder-stage `COPY`
//!   (the #309 break), and
//! - every builder-stage `COPY` source actually exists in the context
//!   (a `COPY` left pointing at a path an unrelated change renamed or
//!   deleted).
//!
//! What neither can see, and what the image build on the PR is for: a
//! missing system dependency in the builder stage, and anything that
//! only shows up once `cargo` actually runs inside the image.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collect every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Extract the literal argument of every `include_str!` / `include_bytes!`
/// invocation in `haystack`.
///
/// Deliberately simple: the macros are always written with a plain string
/// literal in this crate. A `concat!`-built path would not be matched, so
/// if one is ever introduced this parser must grow with it. Doc comments
/// mentioning the macro name are skipped by requiring the `(` and a quote.
fn embedded_paths(haystack: &str) -> Vec<String> {
    let mut found = Vec::new();
    for macro_name in ["include_str!", "include_bytes!"] {
        let mut cursor = 0;
        while let Some(hit) = haystack[cursor..].find(macro_name) {
            let after = cursor + hit + macro_name.len();
            cursor = after;
            let rest = haystack[after..].trim_start();
            let Some(inner) = rest.strip_prefix('(') else {
                continue;
            };
            let inner = inner.trim_start();
            let Some(inner) = inner.strip_prefix('"') else {
                continue;
            };
            let Some(end) = inner.find('"') else {
                continue;
            };
            found.push(inner[..end].to_string());
        }
    }
    found
}

/// Resolve `..` and `.` lexically. The target may not exist yet on disk
/// in a failure case, so `canonicalize` is not usable here.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Source paths copied into the builder stage of the Dockerfile.
///
/// Only the first stage matters: the runtime stage copies the built
/// binary out of the builder with `--from=`, never the source tree.
fn builder_copy_sources(dockerfile: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut stage = 0usize;
    for line in dockerfile.lines() {
        let line = line.trim();
        if line.to_ascii_uppercase().starts_with("FROM ") {
            stage += 1;
            continue;
        }
        if stage != 1 {
            continue;
        }
        let Some(rest) = line
            .strip_prefix("COPY ")
            .or_else(|| line.strip_prefix("copy "))
        else {
            continue;
        };
        let args: Vec<&str> = rest.split_whitespace().collect();
        // `COPY --from=builder ...` pulls from another stage, not the context.
        if args.iter().any(|a| a.starts_with("--from=")) {
            continue;
        }
        let args: Vec<&str> = args.into_iter().filter(|a| !a.starts_with("--")).collect();
        // The last argument is the destination inside the image.
        if args.len() >= 2 {
            sources.extend(args[..args.len() - 1].iter().map(|s| s.to_string()));
        }
    }
    sources
}

/// True when `copied` (a Dockerfile COPY source) brings `target` in.
fn copy_covers(copied: &str, target: &Path) -> bool {
    let copied = copied.trim_end_matches('/');
    if copied == "." {
        return true;
    }
    let copied_path = PathBuf::from(copied);
    target == copied_path.as_path() || target.starts_with(&copied_path)
}

/// True when a `COPY` source is a shell-style pattern rather than a
/// literal path.
///
/// The COPY-source existence check skips patterns instead of resolving
/// them. Reproducing BuildKit's glob semantics is more surface than this
/// test needs, and a false failure here blocks a merge, so the safe
/// direction is to not judge what cannot be resolved cheaply. This
/// repository uses literal paths only; if a pattern is ever introduced,
/// the image build that now runs on every pull request still covers it.
fn is_glob(source: &str) -> bool {
    source.contains(['*', '?', '['])
}

/// Patterns in `.dockerignore` that would strip `target` back out of the
/// context even though a `COPY` names it.
///
/// Supports the subset this repository uses: bare names, directory
/// prefixes (`tests/`), and extension globs (`*.md`). Negations (`!`) are
/// treated as "does not exclude", which is the conservative direction for
/// a test whose failure blocks a merge.
fn dockerignore_hit(patterns: &[String], target: &Path) -> Option<String> {
    let target_str = target.to_string_lossy().replace('\\', "/");
    for raw in patterns {
        let pattern = raw.trim();
        if pattern.is_empty() || pattern.starts_with('#') || pattern.starts_with('!') {
            continue;
        }
        let stripped = pattern.trim_end_matches('/');
        if let Some(ext) = stripped.strip_prefix("*.") {
            if target_str.ends_with(&format!(".{ext}")) {
                return Some(raw.clone());
            }
            continue;
        }
        if target_str == stripped || target_str.starts_with(&format!("{stripped}/")) {
            return Some(raw.clone());
        }
    }
    None
}

#[test]
fn embedded_assets_are_inside_the_docker_build_context() {
    let root = repo_root();

    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    assert!(
        !sources.is_empty(),
        "found no Rust sources under src/, the walker is broken"
    );

    // Repo-relative asset path -> the source file that embeds it.
    let mut assets: BTreeSet<(String, String)> = BTreeSet::new();
    for source in &sources {
        let text = fs::read_to_string(source).unwrap_or_default();
        for literal in embedded_paths(&text) {
            let parent = source.parent().expect("source file has a parent");
            let resolved = normalize(&parent.join(&literal));
            let relative = resolved
                .strip_prefix(&root)
                .unwrap_or(&resolved)
                .to_path_buf();
            let owner = source
                .strip_prefix(&root)
                .unwrap_or(source)
                .to_string_lossy()
                .into_owned();
            assets.insert((relative.to_string_lossy().into_owned(), owner));
        }
    }

    if assets.is_empty() {
        // Nothing is embedded today. The contract is vacuously satisfied,
        // and the test stays in place for the next asset that appears.
        return;
    }

    let dockerfile = fs::read_to_string(root.join("Dockerfile")).expect("Dockerfile must exist");
    let copies = builder_copy_sources(&dockerfile);
    assert!(
        !copies.is_empty(),
        "parsed no COPY sources from the Dockerfile builder stage"
    );

    let ignore_patterns: Vec<String> = fs::read_to_string(root.join(".dockerignore"))
        .map(|text| text.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    let mut failures = Vec::new();
    for (asset, owner) in &assets {
        let asset_path = Path::new(asset);

        assert!(
            root.join(asset_path).exists(),
            "{owner} embeds {asset}, which does not exist on disk"
        );

        if !copies.iter().any(|c| copy_covers(c, asset_path)) {
            let dir = asset_path
                .parent()
                .and_then(|p| p.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| asset.clone());
            failures.push(format!(
                "  {asset}\n    embedded by: {owner}\n    To fix: add `COPY {dir}/ ./{dir}/` to the builder stage of the Dockerfile."
            ));
            continue;
        }

        if let Some(pattern) = dockerignore_hit(&ignore_patterns, asset_path) {
            failures.push(format!(
                "  {asset}\n    embedded by: {owner}\n    A COPY covers it, but .dockerignore pattern `{pattern}` strips it back out.\n    To fix: narrow that pattern or add a `!` negation for this path."
            ));
        }
    }

    let detail = failures.join("\n");
    assert!(
        failures.is_empty(),
        "These embedded assets are unreachable from the Docker build context, so \
         `docker build` fails at compile time even though every other check passes.\n\
         The `docker-check` CI job would catch this too, but only after a 5-6 minute \
         image build and with a raw BuildKit error; this test names the fix directly.\
         \n\n{detail}\n\n\
         Dockerfile builder stage copies: {copies:?}"
    );
}

/// The reverse of the check above: a `COPY` in the builder stage must
/// name a path that is actually in the build context.
///
/// The other direction catches an asset nothing copies. This one catches
/// a `COPY` left pointing at a path that an unrelated change renamed or
/// deleted, which BuildKit rejects before it runs a single build step.
#[test]
fn builder_stage_copy_sources_exist_in_the_context() {
    let root = repo_root();

    let dockerfile = fs::read_to_string(root.join("Dockerfile")).expect("Dockerfile must exist");
    let copies = builder_copy_sources(&dockerfile);
    assert!(
        !copies.is_empty(),
        "parsed no COPY sources from the Dockerfile builder stage"
    );

    let ignore_patterns: Vec<String> = fs::read_to_string(root.join(".dockerignore"))
        .map(|text| text.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    let mut failures = Vec::new();
    for source in &copies {
        if is_glob(source) {
            continue;
        }
        let trimmed = source.trim_end_matches('/');
        if trimmed == "." || trimmed.is_empty() {
            continue;
        }
        let relative = Path::new(trimmed);

        if !root.join(relative).exists() {
            failures.push(format!(
                "  COPY {source}\n    No such path in the build context.\n    \
                 To fix: drop the COPY line, or repoint it at wherever this path moved to."
            ));
            continue;
        }

        if let Some(pattern) = dockerignore_hit(&ignore_patterns, relative) {
            failures.push(format!(
                "  COPY {source}\n    The path exists, but .dockerignore pattern `{pattern}` \
                 strips it back out of the context.\n    \
                 To fix: narrow that pattern or add a `!` negation for this path."
            ));
        }
    }

    let detail = failures.join("\n");
    assert!(
        failures.is_empty(),
        "The Dockerfile builder stage copies paths that are not in the build context. \
         BuildKit rejects such a build before running any build step, so `docker build` \
         fails for everyone the moment this lands.\n\n{detail}"
    );
}

#[test]
fn glob_copy_sources_are_recognised() {
    assert!(is_glob("packaging/*.service"));
    assert!(is_glob("src/**"));
    assert!(is_glob("file?.txt"));
    assert!(is_glob("[abc].txt"));

    assert!(!is_glob("src/"));
    assert!(!is_glob("Cargo.toml"));
    assert!(!is_glob("packaging/systemd/all-smi.service"));
    assert!(!is_glob("."));
}

#[test]
fn copy_coverage_matches_directories_and_exact_files() {
    assert!(copy_covers("src/", Path::new("src/main.rs")));
    assert!(copy_covers("src", Path::new("src/service_cmd/mod.rs")));
    assert!(copy_covers(
        "packaging/",
        Path::new("packaging/systemd/all-smi.service")
    ));
    assert!(copy_covers("Cargo.toml", Path::new("Cargo.toml")));
    assert!(copy_covers(".", Path::new("anything/at/all")));

    assert!(!copy_covers(
        "src/",
        Path::new("packaging/systemd/x.service")
    ));
    assert!(!copy_covers("proto/", Path::new("packaging/x")));
    // A prefix match must respect path boundaries.
    assert!(!copy_covers("pack", Path::new("packaging/x")));
}

#[test]
fn dockerignore_patterns_are_recognised() {
    let patterns: Vec<String> = ["target/", "*.md", "tests/", "!keep.md", "# comment"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert!(dockerignore_hit(&patterns, Path::new("target/release/x")).is_some());
    assert!(dockerignore_hit(&patterns, Path::new("README.md")).is_some());
    assert!(dockerignore_hit(&patterns, Path::new("tests/foo.rs")).is_some());

    assert!(dockerignore_hit(&patterns, Path::new("packaging/systemd/all-smi.service")).is_none());
    assert!(dockerignore_hit(&patterns, Path::new("src/main.rs")).is_none());
    // `target/` must not swallow a path that merely starts with the same text.
    assert!(dockerignore_hit(&patterns, Path::new("targets/x")).is_none());
}

#[test]
fn embedded_path_extraction_finds_literals() {
    let text = r#"
        //! Some docs mentioning include_str! in prose.
        pub const A: &str = include_str!("../../packaging/systemd/all-smi.service");
        pub const B: &[u8] = include_bytes!("assets/logo.png");
    "#;
    let found = embedded_paths(text);
    assert!(found.contains(&"../../packaging/systemd/all-smi.service".to_string()));
    assert!(found.contains(&"assets/logo.png".to_string()));
    assert_eq!(
        found.len(),
        2,
        "prose mention must not be picked up: {found:?}"
    );
}

#[test]
fn builder_stage_copies_are_isolated_from_the_runtime_stage() {
    let dockerfile = "\
FROM rust:1.96-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/all-smi /usr/local/bin/all-smi
COPY runtime-only/ ./runtime-only/
";
    let copies = builder_copy_sources(dockerfile);
    assert_eq!(copies, vec!["Cargo.toml", "Cargo.lock", "src/"]);
}
