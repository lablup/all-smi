# Technical Report: PR #343 - Remove the Dead Vendored OpenSSL Dependency

**Date**: 2026-08-06  
**Status**: Completed  
**Related**: PR #343, Issue #341  
**Risk Level**: Low (dependency removal, no source change)

---

## Executive Summary

PR #343 deletes the two target-conditional `openssl = { features = ["vendored"] }` blocks from `Cargo.toml`. Nothing in the tree uses OpenSSL, and those two entries were the only reason it appeared in the dependency graph at all. Because the release workflow builds `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, and `aarch64-unknown-linux-musl`, which are exactly the targets the two `cfg`s covered, every release had been compiling OpenSSL from source three times for a library nothing links against.

The change is 17 deleted lines across `Cargo.toml` and `Cargo.lock`, with no Rust source touched. It was verified by real cross-target release builds rather than by dependency-graph inspection alone.

---

## 1. Problem Statement

Commit `f1eeb4e` added both blocks while the manifest still declared reqwest 0.12, which resolved with native-tls and therefore carried a live transitive `openssl-sys`. Declaring `openssl` directly with the `vendored` feature forced that to link statically, which suited the musl and cross-compiled aarch64 release binaries.

Commit `3de545d` then moved to reqwest 0.13, which defaults to rustls. The live transitive dependency disappeared, and both blocks have been vestigial since.

The cost was not theoretical. OpenSSL's vendored build compiles the C library from source, and it ran on three of the release matrix's targets on every tagged release, for a dependency with no consumer.

## 2. Change Summary

| Item | Value |
|------|-------|
| Files changed | 2 |
| Lines added | 0 |
| Lines deleted | 17 |
| Rust source changed | No |
| Tests added | 0 |

### Files

| File | Purpose |
|------|---------|
| `Cargo.toml` | Removes the `cfg(target_env = "musl")` and `cfg(all(target_arch = "aarch64", target_env = "gnu"))` dependency blocks. |
| `Cargo.lock` | Drops `openssl` from the `all-smi` dependency list and removes the `openssl-src` package entry plus its edge from `openssl-sys`. |

## 3. Technical Decisions

### 3.1 Confirm the claim independently rather than reading the manifest

"Nothing uses it" is the sort of premise that is easy to assert and expensive to get wrong, since a mistake here breaks TLS in release binaries only, on targets the development machine does not build. Two independent confirmations were taken before deleting anything:

- `grep -rn openssl src/ --include='*.rs'` returns nothing.
- Before the change, `cargo tree -i openssl` reported `all-smi` itself as the sole reverse dependency on `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`, and `--target all`, while returning nothing at all on `x86_64-unknown-linux-gnu`.

The second is the decisive one: the crate was in the graph because this manifest put it there, not because anything pulled it in.

### 3.2 "Gone from Cargo.lock" is the wrong success check

`openssl`, `openssl-sys`, `foreign-types`, and `native-tls` remain recorded in `Cargo.lock` after this change. That is expected rather than a leftover: the lockfile records the union of the graph including feature-gated edges, and `furiosa-smi-rs -> attohttpc -> native-tls -> openssl` keeps them listed even though that edge activates for no target this project builds.

The correct check is `cargo tree --target all -i openssl` returning nothing, which it now does.

### 3.3 Leave `openssl-probe` alone

Despite the name, `openssl-probe` has nothing to do with the `openssl` crate. It is live through `reqwest -> rustls-platform-verifier -> rustls-native-certs -> openssl-probe` and still compiles in every Linux build, which is the expected rustls behavior. Removing it because the name matched would have broken certificate discovery.

## 4. Validation Results

Cross-target builds were run in Linux containers from the branch, because `cargo tree` is an argument and not a build. Each produced a real release binary and none compiled `openssl-sys`.

| Target | Result | Time | Binary | `openssl-*` build dirs |
|--------|--------|------|--------|------------------------|
| `aarch64-unknown-linux-gnu` | built | 2m58s | 9.6 MB | none |
| `aarch64-unknown-linux-musl` | built | 2m40s | 9.0 MB | none |
| `x86_64-unknown-linux-musl` | built (emulated amd64) | 19m28s | 12.0 MB | none |

- `cargo tree --target all -i openssl` returns nothing, and likewise for each of the three targets individually.
- `cargo tree --target all -i openssl-probe` still resolves through `rustls-native-certs`, confirming the live path survived.

## 5. Outcome and Follow-up

- PR #343 was squash-merged into `main` as `59b3c9c`.
- Issue #341 closed automatically through the PR's `Closes #341` link.
- Release build time on the three affected targets drops by the cost of a vendored OpenSSL compile, which the measurements above bound but do not isolate, since they are whole-build numbers rather than a before/after pair on the same host.
- No follow-up work is outstanding. If a future dependency reintroduces a live `openssl-sys` edge, the `vendored` decision has to be made again on its own merits rather than restored from this history.

---

## Appendix: Key Terms

| Keyword | Description | Relevance |
|---------|-------------|-----------|
| `vendored` feature | Builds OpenSSL from bundled C source instead of linking the system library | The reason the dead dependency was expensive rather than merely present |
| `cargo tree -i` | Inverse dependency query: who depends on this crate | The check that proved `all-smi` was the only reverse dependency |
| feature-gated lockfile edge | `Cargo.lock` records edges that activate for no target | Why `openssl` stays in the lockfile after removal |
