# Developer Documentation

This guide provides comprehensive information for developers and contributors working on all-smi.

## Table of Contents

- [Development Environment Setup](#development-environment-setup)
- [Building from Source](#building-from-source)
- [Development Workflow](#development-workflow)
- [Testing](#testing)
- [Code Style and Standards](#code-style-and-standards)
- [Mock Server Development](#mock-server-development)
- [Docker Development](#docker-development)
- [CI/CD Process](#cicd-process)
- [Platform-Specific Development](#platform-specific-development)
- [Contributing Guidelines](#contributing-guidelines)
- [Debugging Tips](#debugging-tips)

## Development Environment Setup

### Prerequisites

#### Required Tools
- **Rust**: 1.88 or later (install via [rustup](https://rustup.rs/))
- **Cargo**: Comes with Rust installation
- **Git**: For version control
- **protoc**: Protocol buffer compiler (only required for Linux builds with Tenstorrent support)

#### Platform-Specific Requirements

**Linux:**
```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev protobuf-compiler

# Fedora/RHEL
sudo dnf install pkg-config openssl-devel protobuf-compiler protobuf-devel

# Arch Linux
sudo pacman -S pkg-config openssl protobuf
```

**macOS:**
```bash
# Install Homebrew if not present
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# No additional dependencies required for macOS
# Note: protobuf is NOT needed on macOS as Tenstorrent NPU support is Linux-only
```

**Windows:**
- Not officially supported, but may work with WSL2

### Setting Up the Repository

```bash
# Clone the repository
git clone https://github.com/inureyes/all-smi.git
cd all-smi

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Verify installation
cargo --version
rustc --version
```

## Building from Source

### Standard Build

```bash
# Debug build (faster compilation, slower runtime)
cargo build

# Release build (optimized for production)
cargo build --release

# Build specific binary
cargo build --release --bin all-smi

# Build with mock server feature
cargo build --release --bin all-smi-mock-server --features="mock"
```

### Optional Build Features

The default build (`default = ["cli", "amd"]`) includes the full CLI, TUI, API server, and the AMD GPU backend. Cargo features:

| Feature | Default | Purpose |
|---------|---------|---------|
| `cli` | on | CLI parsing, TUI (`crossterm`), and the `axum` API server. Disable for a lean library-only build. |
| `amd` | on | AMD GPU backend on glibc Linux via the `libamdgpu_top` crate. Disable to drop the `libdrm.so.2` / `libdrm_amdgpu.so.1` runtime dependency. |
| `mock` | off | Builds the `all-smi-mock-server` binary that simulates GPU/NPU clusters. |
| `furiosa` | off | Furiosa NPU backend via the `furiosa-smi-rs` crate (Linux targets). |
| `level_zero` | off on Linux, **always on for Windows targets** | Intel oneAPI Level Zero (Sysman) backend for Intel client GPUs. Dynamically loads `libze_loader.so.1` (Linux) / `ze_loader.dll` (Windows) at runtime; a missing runtime degrades silently to the sysfs/WMI baseline. On Windows the backend is compiled in regardless of this flag (see below), so `features:` in a support bundle cannot tell you whether it is present; read `level_zero:` in `version.txt` instead. |

```bash
# Example: build with the Intel Level Zero backend enabled (Linux)
cargo build --release --features level_zero
```

##### Why Windows ignores the flag

`build.rs` emits an `all_smi_level_zero` cfg alias, and every consumer gates
on that rather than on the cargo feature. The alias is on for any Windows
target whether or not `--features level_zero` was passed.

Three reasons. The backend adds no dependency: it `dlopen`s `ze_loader.dll`
through `libloading`, already an unconditional Windows dependency, so
compiling it in adds no import and no startup cost on a machine without the
DLL. Nothing else on Windows can supply GPU temperature, power, or
frequency, so leaving it out makes those columns permanently empty on Intel
hardware. And we publish a single `x86_64-pc-windows-msvc` artifact; an
opt-in backend would mean shipping an Intel and a non-Intel Windows package.

Cargo cannot express a per-target feature default, which is why this is a
cfg alias rather than a `[target.'cfg(windows)'.features]` entry.

#### Dropping the AMD backend (`amd`)

`libamdgpu_top` pulls in `libdrm_amdgpu_sys`, which links `libdrm.so.2` and `libdrm_amdgpu.so.1` unconditionally. Those become hard `NEEDED` entries on every Linux binary that links this crate, so a host without AMD's userspace DRM libraries fails to start with a loader error before `main` runs, which the program cannot catch or report. Turning `amd` off removes the dependency and both `NEEDED` entries.

```bash
# Library-only build with no AMD backend and no libdrm linkage
cargo build --release --no-default-features

# CLI without the AMD backend: --no-default-features also drops `cli`,
# so re-enable it explicitly
cargo build --release --no-default-features --features cli
```

For a downstream crate the same rule applies. `default-features = false` turns off `cli` as well as `amd`, so a consumer that wants the CLI but not AMD must ask for `cli` back:

```toml
[dependencies]
all-smi = { version = "0.25", default-features = false, features = ["cli"] }
```

Verify the result with `objdump -p target/release/all-smi | grep NEEDED`; neither `libdrm.so.2` nor `libdrm_amdgpu.so.1` should appear.

The musl release artifacts (`all-smi-linux-x86_64-musl`, `all-smi-linux-aarch64-musl`) have never included AMD support and stay the simplest option for minimal containers. `all-smi doctor` reports which gate applied: `amd.build.target_env` and `amd.libamdgpu_top.abi` distinguish a musl build from a glibc build without the `amd` feature, and `doctor --bundle` lists the enabled features. Windows AMD support goes through ADL/WMI and is unaffected by this feature.

### Platform-Specific Builds

#### Linux with musl (for static linking)
```bash
# Install musl target
rustup target add x86_64-unknown-linux-musl

# Build static binary
cargo build --release --target x86_64-unknown-linux-musl
```

#### Cross-compilation for ARM64
```bash
# Install ARM64 target
rustup target add aarch64-unknown-linux-gnu

# Build for ARM64
cargo build --release --target aarch64-unknown-linux-gnu
```

### Build Troubleshooting

If you encounter build errors:

1. **OpenSSL Issues (musl/aarch64)**: The project automatically uses vendored OpenSSL for these targets
2. **Protobuf Errors**: Ensure protoc is installed and in PATH (Linux only, required for Tenstorrent NPU support)
3. **Dependency Resolution**: Run `cargo clean` and rebuild

## Development Workflow

### Quick Start Commands

The project includes a Makefile for common development tasks:

```bash
# Run local monitoring
make local

# Run remote view mode
make remote

# Start API server
make api

# Run mock server
make mock

# Run tests
make test

# Run linting
make lint

# Build release version
make release

# Clean build artifacts
make clean
```

### Development Cycle

1. **Make Changes**: Edit source files in `src/`
2. **Check Format**: `cargo fmt`
3. **Run Linting**: `cargo clippy`
4. **Run Tests**: `cargo test`
5. **Build & Test**: `cargo run -- local` (or other commands)
6. **Commit Changes**: Follow conventional commit format

### Running During Development

```bash
# Run in local mode (may require sudo on macOS)
cargo run --bin all-smi -- local

# Run in API mode
cargo run --bin all-smi -- api --port 9090

# Run in view mode with mock servers
SUPPRESS_LOCALHOST_WARNING=1 cargo run --bin all-smi -- view --hostfile ./hosts.csv

# Run mock server
cargo run --features mock --bin all-smi-mock-server -- --port-range 10001-10010
```

## Testing

### Running Tests

```bash
# Run all unit tests (no sudo required)
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_name

# Run tests in single thread (useful for debugging)
cargo test -- --test-threads=1
```

### Platform-Specific Testing

**macOS Tests Requiring sudo:**
```bash
# Run all tests including those requiring sudo
sudo cargo test -- --include-ignored --test-threads=1

# Skip sudo tests explicitly
SKIP_SUDO_TESTS=1 cargo test
```

### Test Categories

- **Unit Tests**: Testing individual functions and modules
- **Integration Tests**: Testing component interactions
- **Mock Server Tests**: Testing with simulated GPU environments

For comprehensive testing documentation, see [TESTING.md](TESTING.md).

### Benchmarking Local-Mode Collection Cost

`scripts/bench-local-interval.sh` measures how much CPU `all-smi local` uses at different collection intervals. Use it when changing the collection cadence, the sampling strategy, or anything a device reader does on every poll.

```bash
cargo build --release --bin all-smi
scripts/bench-local-interval.sh                    # default: 60s window, intervals 1/2/3
scripts/bench-local-interval.sh -d 120 -i "2 5"    # longer window, custom intervals
scripts/bench-local-interval.sh -c 5-9,15-19 -r 3  # pin to one core cluster, average 3 windows
scripts/bench-local-interval.sh -b ~/bin/all-smi   # measure a binary built elsewhere
scripts/bench-local-interval.sh -h                 # full usage and flag list
```

It requires `tmux` (macOS and Linux only) so the TUI runs detached at a fixed 200x50 size; terminal size affects render cost, so fixing it is what makes results from different machines comparable. CPU is computed from the process CPU-time delta rather than `ps -o %cpu`, because that column is a decaying recent average on macOS and a lifetime average on Linux. Results are percent of one core.

On Linux, building needs `libdrm-dev`. Without it the link fails on `-ldrm` and `-ldrm_amdgpu` even on a host with no AMD GPU, because `libamdgpu_top` is a hard dependency of the glibc Linux target.

The script prints an environment block (all-smi version, OS, CPU model and core topology, affinity, GPU, process count, terminal size, window length) above the numbers. Include it whenever you report results: collection cost depends heavily on which device readers are active, so numbers without that context cannot be compared.

#### Comparing Results Across Machines

"Percent of one core" is not a single quantity on a heterogeneous CPU. ARM big.LITTLE parts, Intel P/E hybrids, and Apple Silicon all mix core types, and identical work costs a different amount of CPU time on each type. On an NVIDIA GB10 (Cortex-X925 at 3.9 GHz plus Cortex-A725 at 2.8 GHz), pinning the same run to one cluster or the other moves the result by about 1.5x, which is larger than the interval effect the script is usually being used to measure. Unpinned runs land somewhere between the two depending on where the scheduler put the threads, which is also why they vary more between repeats.

So the durable number is the **ratio between two intervals measured on one host**, not the absolute percentage. Compare absolute percentages across machines only when both reports state their core placement. The `topology` and `affinity` lines in the environment block carry that context automatically.

On a heterogeneous host, prefer `-c` to pin one cluster and say which one you pinned; it also cuts run-to-run variance substantially. Use `-r` to average several windows when the effect you are chasing is close to the spread between repeats. Two limits on `-c`: it is Linux only, since macOS offers no way to select a specific CPU set (only E-core confinement is reachable at all, via `taskpolicy -b`, which also changes scheduling priority); and it should be run on bare metal, because inside a container without a cpuset `all-smi` sizes its own CPU view from `sched_getaffinity`, so pinning would shrink the set of cores it parses and renders rather than only relocating the same work. Background and measurements are in issue #290.

Coverage is currently thin outside Apple Silicon. See issue #288 if you have Linux or Windows hardware and can contribute measurements.

## Code Style and Standards

### Formatting

The project uses rustfmt for consistent code formatting:

```bash
# Format all code
cargo fmt

# Check formatting without changes
cargo fmt --check
```

### Linting

Clippy is used for catching common mistakes and improving code quality:

```bash
# Run clippy with warnings as errors
cargo clippy -- -D warnings

# Run clippy with all features
cargo clippy --all-features -- -D warnings
```

### Code Style Guidelines

1. **Error Handling**: Use `Result<T, E>` and `anyhow` for error propagation
2. **Async Code**: Use `tokio` for async runtime, follow async best practices
3. **Documentation**: Document public APIs and complex logic
4. **Testing**: Write unit tests for new functionality
5. **Performance**: Profile before optimizing, avoid premature optimization

### Configured Lints

The project enforces these Clippy lints (see `Cargo.toml`):
- `uninlined_format_args`: Warn on format string inefficiencies
- `needless_return`: Avoid unnecessary return statements
- `redundant_closure`: Simplify closure usage
- `manual_range_contains`: Use idiomatic range checks
- `module_inception`: Avoid module naming confusion
- `bool_comparison`: Use idiomatic boolean checks

## Configuration Architecture

Issue #192 introduced TOML config file support. The runtime configuration flows through three layers:

1. **Compiled defaults** — constants in `src/common/config.rs` and the `Default` impls on each `Settings` sub-struct in `src/common/config_file.rs`.
2. **TOML file + env overrides** — merged by `config_file::load(path)` into a `Settings` struct. Env vars follow `ALL_SMI_<SECTION>_<KEY>` canonical naming; legacy aliases (`ALL_SMI_ENERGY_PRICE`, `ALL_SMI_ALERT_TEMP`, etc.) remain supported for backward compatibility.
3. **CLI overrides** — applied in `src/main.rs` after the runtime starts. Every existing CLI field is now `Option<T>`; `None` means "fall through to `Settings`". This preserves the pre-existing flag surface while letting config operators omit them.

Key files:

| File | Purpose |
|------|---------|
| `src/common/config_file.rs` | Schema + loader. Produces `Settings` from file + env. |
| `src/common/config_file_tests.rs` | Unit tests (in a sibling file so the implementation stays under the 500-line soft cap). |
| `src/common/paths.rs` | Platform-aware config directory resolution and `~` expansion. |
| `src/common/secure_write.rs` | Shared `O_NOFOLLOW` + `0o600` writer, reused by `config init`, snapshot, and record. |
| `src/config_cmd/` | Runtime glue for the `config init/print/validate` subcommands. |
| `tests/config_file_integration_test.rs` | Full precedence / backward-compat integration tests. |

When adding a new subcommand option that should be config-file-driven:

1. Add the field to the appropriate section in `src/common/config_file.rs` (both the `*Section` deserializer and the merged `*Settings` runtime struct).
2. Add the default in `impl Default for Settings`.
3. Plumb the env-var override in `apply_env` (canonical name `ALL_SMI_<SECTION>_<KEY>`).
4. Update `src/config_cmd/render.rs` to include the field in `print` output.
5. Update `src/config_cmd/example.rs` (the commented example written by `config init`).
6. In `src/main.rs` apply the fallback: `args.field.or(settings.section.field)`.

## Mock Server Development

The mock server simulates GPU environments for testing:

### Running Mock Server

```bash
# Basic usage
cargo run --features mock --bin all-smi-mock-server

# With specific options
cargo run --features mock --bin all-smi-mock-server -- \
  --port-range 10001-10010 \
  --gpu-name "NVIDIA H200 141GB HBM3" \
  -o hosts.csv
```

### Mock Server Options

- `--port-range`: Specify port range for multiple instances
- `--gpu-name`: Set custom GPU name for simulation
- `--gpu-count`: Number of GPUs per node (default: 8)
- `--failure-rate`: Simulate connection failures (0.0-1.0)
- `-o, --output`: Generate hosts file for view mode

### Testing with Mock Server

```bash
# Start mock servers
./target/release/all-smi-mock-server --port-range 10001-10128 -o hosts.csv &

# Monitor mock servers
SUPPRESS_LOCALHOST_WARNING=1 ./target/release/all-smi view --hostfile hosts.csv --interval 1
```

## Docker Development

### Development Container

```bash
# Run interactive development container
make docker-dev

# Inside container:
cargo build --release
cargo test
./target/release/all-smi local
```

### Testing in Docker

```bash
# Test API mode in container
make docker-test-container-api

# Test view mode in container
make docker-test-container-view
```

> **Note:** These targets run all-smi inside a stock `rust:1.88` container with the source bind-mounted, to exercise its container-awareness code paths. The project does not build or publish a container image of its own. See [Installation](README.md#installation) for the supported ways to install all-smi.

### Docker Development Tips

1. **Cache Management**: The Makefile creates `.cargo-cache` for faster rebuilds
2. **Resource Limits**: Containers are limited to 2-4GB RAM and 1.5-2.5 CPUs
3. **Volume Mounts**: Source code is mounted for live development
4. **Base Image**: Uses `rust:1.88` for consistency

## CI/CD Process

### Continuous Integration

The project uses GitHub Actions for CI:

1. **Test Suite**: Runs on every push and PR
   - Unit tests
   - Format checking (`cargo fmt`)
   - Linting (`cargo clippy`)

2. **Build Check**: Verifies release build

### Release Process

Releases are automated via GitHub Actions:

1. Tag a release: `git tag v0.9.0`
2. Push tag: `git push origin v0.9.0`
3. GitHub Actions builds and publishes:
   - Binary releases for multiple platforms
   - Debian/Ubuntu packages
   - Homebrew formula updates

### Platform Builds

The CI builds for these platforms:
- Linux x86_64 (glibc and musl)
- Linux aarch64
- macOS x86_64
- macOS aarch64 (Apple Silicon)

## Platform-Specific Development

### NVIDIA GPU Support

- Uses `nvml-wrapper` for direct NVML access
- Falls back to `nvidia-smi` parsing when NVML unavailable
- Located in `src/gpu/nvidia.rs`

### Apple Silicon Support

- Uses `powermetrics` for hardware metrics (requires sudo)
- Metal framework integration for GPU info
- Located in `src/gpu/apple_silicon.rs`

### NPU Support

**Intel Gaudi NPUs (Linux only):**
- Uses `hl-smi` command running as a background process
- Supports Gaudi 1, Gaudi 2, and Gaudi 3 generations
- Supports PCIe, OAM, UBB, and HLS form factors
- Located in `src/device/hlsmi/` (manager, parser, config, store, process)
- Reader implementation in `src/device/readers/gaudi.rs`
- Automatic device name mapping (e.g., HL-325L → Intel Gaudi 3 PCIe LP)
- Follows same background process pattern as Apple Silicon's PowerMetrics

**Tenstorrent NPUs (Linux only):**
- Uses `luwen` library for telemetry
- Supports Grayskull, Wormhole, Blackhole architectures
- Located in `src/gpu/tenstorrent.rs`
- Requires `protobuf-compiler` on Linux for building

**Rebellions NPUs:**
- Uses `rbln-stat` command
- Supports ATOM, ATOM+, ATOM Max
- Located in `src/gpu/rebellions.rs`

**Furiosa NPUs:**
- Uses `furiosa-smi-rs` crate (optional dependency)
- Supports RNGD architecture
- Located in `src/gpu/furiosa.rs`

### NVIDIA Jetson Support

- Special handling for Tegra-based systems
- DLA (Deep Learning Accelerator) monitoring
- Located in `src/gpu/nvidia_jetson.rs`

## Contributing Guidelines

### Before Contributing

1. **Check Issues**: Look for existing issues or create a new one
2. **Discussion**: For major changes, discuss first in an issue
3. **Fork & Branch**: Work in a feature branch

### Making Changes

1. **Follow Style**: Use `cargo fmt` and `cargo clippy`
2. **Write Tests**: Add tests for new functionality
3. **Update Docs**: Keep documentation current
4. **Test Thoroughly**: Run full test suite

### Submitting Pull Requests

1. **Clear Title**: Use conventional commit format
   - `feat:` New feature
   - `fix:` Bug fix
   - `refactor:` Code restructuring
   - `docs:` Documentation changes
   - `test:` Test additions/changes

2. **Description**: Explain what and why
3. **Link Issues**: Reference related issues
4. **Pass CI**: Ensure all checks pass

### Code Review Process

- PRs require at least one review
- Address feedback constructively
- Keep PRs focused and manageable
- Squash commits before merge when appropriate

## Debugging Tips

### Common Issues and Solutions

#### PowerMetrics on macOS
```bash
# Check if powermetrics is available
which powermetrics

# Test powermetrics manually
sudo powermetrics --samplers gpu_power -i 1000 -n 1
```

#### NVIDIA GPU Detection
```bash
# Check NVIDIA driver
nvidia-smi

# Check NVML availability
ldd target/release/all-smi | grep nvidia
```

#### Connection Issues in Remote Mode
```bash
# Test API endpoint
curl http://localhost:9090/metrics

# Check system limits (macOS)
sysctl kern.ipc.somaxconn

# Suppress localhost warning
export SUPPRESS_LOCALHOST_WARNING=1
```

### Debug Logging

```bash
# Enable debug logging
RUST_LOG=debug cargo run -- local

# Enable trace logging for specific module
RUST_LOG=all_smi::gpu=trace cargo run -- local

# Enable network debugging
RUST_LOG=reqwest=debug cargo run -- view --hosts http://localhost:9090
```

### Performance Profiling

```bash
# Build with debug symbols
cargo build --release

# Profile with instruments (macOS)
instruments -t "Time Profiler" target/release/all-smi

# Profile with perf (Linux)
perf record -g target/release/all-smi
perf report
```

### Memory Leak Detection

```bash
# Using valgrind (Linux)
valgrind --leak-check=full target/release/all-smi

# Using leaks (macOS)
leaks --atExit -- target/release/all-smi
```

## Additional Resources

- [README.md](README.md) - User documentation and feature overview
- [TESTING.md](TESTING.md) - Comprehensive testing guide
- [API.md](API.md) - Prometheus metrics API documentation
- [Rust Book](https://doc.rust-lang.org/book/) - Rust language documentation
- [Tokio Docs](https://tokio.rs/) - Async runtime documentation

## Getting Help

- **Issues**: [GitHub Issues](https://github.com/inureyes/all-smi/issues)
- **Discussions**: Use GitHub Discussions for questions
- **Documentation**: Check docs/ directory for additional guides

## License

This project is licensed under the Apache License 2.0. See [LICENSE](LICENSE) for details.