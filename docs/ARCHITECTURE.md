# All-SMI Architecture Documentation

## Executive Summary

All-SMI is a unified GPU/NPU monitoring tool that provides both terminal UI (TUI) and Prometheus metrics interface for monitoring local and remote accelerator resources. Built with a modular, event-driven architecture on async Rust patterns, it supports multiple platforms (NVIDIA GPU, AMD GPU, Apple Silicon, Jetson, Intel Arc/Iris Xe client GPU, Intel Gaudi, Tenstorrent, Rebellions, Furiosa) and enables distributed monitoring across multiple nodes.

## Table of Contents

1. [Overall Architecture](#overall-architecture)
2. [Core Design Principles](#core-design-principles)
3. [Module Organization](#module-organization)
4. [Data Flow Architecture](#data-flow-architecture)
5. [Core Abstractions and Traits](#core-abstractions-and-traits)
6. [Platform-Specific Implementations](#platform-specific-implementations)
7. [Networking and Remote Monitoring](#networking-and-remote-monitoring)
8. [Terminal UI Implementation](#terminal-ui-implementation)
9. [Performance and Concurrency](#performance-and-concurrency)
10. [Error Handling and Recovery](#error-handling-and-recovery)
11. [Security Architecture](#security-architecture)
12. [Configuration Management](#configuration-management)
13. [Build System and Features](#build-system-and-features)
14. [Strategy Pattern Implementation](#strategy-pattern-implementation)
15. [Testing Strategy](#testing-strategy)
16. [Future Roadmap](#future-roadmap)

## Overall Architecture

### System Modes

All-SMI operates in three distinct modes:

1. **Local Mode**: Terminal UI monitoring local hardware directly
2. **API Mode**: Headless server exposing Prometheus metrics endpoints
3. **View Mode**: Distributed monitoring client aggregating remote instances

### Key Design Patterns

The architecture extensively leverages proven design patterns:

- **Strategy Pattern**: Platform-specific device readers with unified interfaces
- **Factory Pattern**: Runtime device reader selection based on hardware detection
- **Producer-Consumer**: Async data collection tasks feed shared state consumed by UI/API
- **Command Pattern**: Structured TUI event handling and processing
- **Builder Pattern**: Configuration and client construction
- **Trait Object Pattern**: Dynamic dispatch for cross-platform hardware abstractions

## Core Design Principles

### 1. Platform Abstraction
- **Trait-based design**: All platform-specific implementations adhere to common traits
- **Runtime detection**: Automatic platform detection selects appropriate readers
- **Extensibility**: New platforms can be added by implementing the reader traits
- **Zero-cost abstractions**: Compile-time optimizations eliminate runtime overhead

### 2. Asynchronous Architecture
- **Tokio runtime**: Full-featured async runtime for complex operations
- **Task separation**: Data collection and UI rendering run independently
- **Non-blocking I/O**: All operations use async/await patterns
- **Shared state**: `Arc<Mutex<AppState>>` enables thread-safe communication

### 3. Modular Design
- **Clean boundaries**: Well-defined module interfaces and responsibilities
- **Dependency injection**: Components receive dependencies through constructors
- **Plugin architecture**: Easy extension through trait implementations
- **Single responsibility**: Each module handles one aspect of functionality

## Module Organization

### Directory Structure

```
src/
├── traits/           # Core trait definitions (Strategy interfaces)
├── device/           # Hardware abstraction layer
│   ├── readers/      # Platform-specific implementations
│   └── common/       # Shared device utilities
├── metrics/          # Data aggregation and analysis
├── view/             # TUI orchestration and lifecycle
│   └── data_collection/  # Strategy pattern implementation
├── ui/               # Rendering and widget system
├── api/              # Web server and HTTP handlers
├── network/          # HTTP client and remote polling
├── parsing/          # Command output processing
├── storage/          # Disk information modeling
├── utils/            # System utilities and helpers
└── common/           # Shared application concerns
```

### Module Responsibilities

- **Device Layer**: Hardware abstraction through traits (`GpuReader`, `CpuReader`, `MemoryReader`)
- **Metrics Layer**: Data aggregation, trend analysis, and health monitoring
- **View Layer**: Application lifecycle management and async task coordination
- **UI Layer**: Terminal rendering with differential updates
- **Network Layer**: Robust HTTP client with retry logic and security validations
- **Parsing Layer**: Efficient text processing with macro-based DSL
- **Storage Layer**: Disk usage monitoring and reporting
- **Utils Layer**: Cross-cutting concerns and helper functions

## Data Flow Architecture

### Local Mode Flow
```
DataCollector (background) → Device Readers → Shared AppState → UI Loop (foreground)
```

### Remote Mode Flow
```
NetworkClient → Remote /metrics endpoints → MetricsParser → Shared AppState → UI Loop
```

### API Mode Flow
```
DataCollector → Device Readers → API State → HTTP Handlers → Prometheus Export
```

### Unix Domain Socket Support

API mode supports Unix Domain Sockets (UDS) as an alternative to TCP for local IPC scenarios:

#### Benefits
- **No port management**: Avoid port conflicts with other services
- **Better security**: File permission-based access control (0600)
- **Lower overhead**: Bypasses TCP/IP stack for local communication
- **Simplified discovery**: Fixed socket path instead of dynamic ports

#### Implementation Details
- **Listener options**: TCP only, UDS only, or both simultaneously
- **Platform defaults**:
  - Linux: `/var/run/all-smi.sock` (fallback to `/tmp/all-smi.sock`)
  - macOS: `/tmp/all-smi.sock`
- **Socket lifecycle**:
  - Stale socket removal on startup (atomic, TOCTOU-safe)
  - Graceful cleanup on shutdown via signal handlers
- **Security**: Socket permissions set to `0600` immediately after bind
- **Platform availability**: Unix only (Linux, macOS); Windows pending Rust ecosystem support

#### Architecture
```
Client (curl/Python) → Unix Socket → Axum Server → Same handlers as TCP
```

### State Management

The architecture uses `Arc<Mutex<AppState>>` for thread-safe state sharing:
- **Producers**: Background tasks collecting hardware metrics
- **Consumers**: UI renderer or HTTP handlers reading state
- **Synchronization**: Mutex ensures data consistency
- **Performance**: Minimal lock contention through quick updates

## Core Abstractions and Traits

### Primary Trait Definitions

```rust
// Core data collection abstraction
pub trait DataCollector: Send + Sync {
    type Data;
    async fn collect(&self) -> Result<Self::Data>;
}

// Device-specific readers
pub trait GpuReader: Send + Sync {
    fn get_gpu_info(&self) -> Vec<GpuInfo>;
    fn get_process_info(&self) -> Vec<ProcessInfo>;
}

pub trait CpuReader: Send + Sync {
    fn get_cpu_info(&self) -> Vec<CpuInfo>;
}

pub trait MemoryReader: Send + Sync {
    fn get_memory_info(&self) -> Vec<MemoryInfo>;
}

// Export formatting strategy
pub trait MetricsExporter: Send + Sync {
    fn export(&self, data: &MetricsData, format: ExportFormat) -> Result<String>;
}
```

### Trait Design Principles

- **Associated Types**: Enable zero-cost abstractions while maintaining type safety
- **Send + Sync Bounds**: Ensure thread safety for async environments
- **Strategy Composition**: Traits can be combined for complex behaviors
- **Error Propagation**: Consistent Result types for error handling

## Platform-Specific Implementations

### GPU Reader Implementations

#### Apple Silicon (`src/device/readers/apple_silicon.rs`)
- Uses `powermetrics` command for hardware metrics
- Integrates Metal framework for GPU utilization
- Requires sudo privileges for hardware access
- Provides unified memory metrics

#### NVIDIA (`src/device/readers/nvidia.rs`)
- Primary: NVML library for direct GPU access
- Fallback: `nvidia-smi` command parsing
- Supports multi-GPU configurations
- Includes CUDA process tracking

#### NVIDIA Jetson (`src/device/readers/nvidia_jetson.rs`)
- Specialized for Tegra platforms
- DLA (Deep Learning Accelerator) support
- Integrated memory architecture handling

#### Intel Arc / Iris Xe client GPU (Linux: `src/device/readers/intel_gpu_linux.rs`, Windows: `src/device/readers/intel_gpu_windows.rs`)
- Linux: sysfs discovery under `/sys/class/drm/card*`, vendor `0x8086`, driver `i915` or `xe`
- Windows: WMI `Win32_VideoController` query filtered to Intel graphics adapter names
- PCI device-ID to marketing-name table in `src/device/readers/intel_gpu_names.rs` covering Arc A/B-series, Iris Xe, Xe-LPG (Core Ultra / Meteor Lake), and Xe2 (Lunar Lake)
- Architecture classification (Alchemist, Battlemage, XeLpg, XeLpgPlus, IrisXe, OlderIntegrated) with SYCL/oneAPI capability flag
- Discrete VRAM tracking via i915 `mem_info_vram_total` or xe `tile0/vram0/total_bytes`; integrated GPUs report zero
- Engine-busy utilization via `src/device/readers/intel_gpu_engine.rs`: walks sysfs per-engine `busy`/`busy_ns` monotonic counters exposed by both i915 (flat and nested layouts) and xe (single-GT and multi-GT layouts), delta-computes per-engine percentages each refresh, and surfaces `max(render, compute)` as `GpuInfo.utilization`. The first refresh per card is a seeding call returning `0.0`; real values appear from the second refresh. When the kernel exposes no engine counters, `detail["Utilization"]` carries an explanatory note and `utilization` stays `0.0`. The PMU `perf_event_open(2)` fallback used by `intel_gpu_top` on older kernels is deferred to future work.
- Per-process GPU memory accounting via `src/device/readers/intel_gpu_fdinfo.rs`: walks `/proc/<pid>/fd/` to find file descriptors opened against an Intel DRM minor (`cardN` or `renderD<M>`), parses the matching `/proc/<pid>/fdinfo/<fd>` block for `drm-resident-{local0,system}` (i915) or `drm-resident-{vram0,gtt}` (xe), and dedupes shared fds by `drm-client-id` so processes that `dup(2)` a DRM fd are not double-counted. Permission errors degrade silently per-process. Per-process engine-time utilization (delta-tracked counters keyed on `drm-client-id`) is deferred to a follow-up.
- Opt-in Level Zero (oneAPI) augmentation via `src/device/readers/intel_gpu_level_zero.rs` (gated on `--features level_zero`, default off): dynamically loads `libze_loader.so.1` (Linux) or `ze_loader.dll` (Windows) via `libloading`, sets `ZES_ENABLE_SYSMAN=1` once before `zeInit`, enumerates Intel devices by PCI BDF, and surfaces per-engine activity (RENDER_SINGLE, COMPUTE_SINGLE — the XMX class on Arc and Battlemage — COPY_SINGLE, MEDIA_DECODE_SINGLE, MEDIA_ENCODE_SINGLE) plus card-level power derived from delta-tracked `zesPowerGetEnergyCounter` readings. On Linux the sysfs engine counters remain the source of truth for `GpuInfo.utilization`; L0 adds the XMX activity sysfs cannot reach and the `Power (L0)` detail entry, and flips `detail["Metrics Source"]` from `"sysfs (engine counters)"` to `"sysfs + Level Zero"`. On Windows L0 overwrites the WMI zeros for `GpuInfo.utilization` and `GpuInfo.power_consumption` and flips `detail["Metrics Source"]` from `"WMI"` to `"WMI + Level Zero"`. Hosts without the L0 loader installed (or builds without the feature) behave exactly as the sysfs / WMI baseline does today: no panic, no log spam, one `tracing::debug!` line at startup. Temperature, frequency, memory state, per-process L0 stats, RAS error reporting, performance factor, and fine-grained power-limit control are explicitly deferred to follow-up issues (see #248).

#### Intel Gaudi (`src/device/readers/gaudi.rs`, `src/device/hlsmi/`)
- Uses `hl-smi` command running as a background process
- Supports Gaudi 1, Gaudi 2, and Gaudi 3 generations
- Form factor support: PCIe, OAM, UBB, HLS
- Automatic device name mapping (e.g., HL-325L → Intel Gaudi 3 PCIe LP)
- Background process manager (`src/device/hlsmi/manager.rs`)
- CSV output parser (`src/device/hlsmi/parser.rs`)
- Circular buffer for metrics storage (`src/device/hlsmi/store.rs`)
- Follows the same design pattern as Apple Silicon's PowerMetrics integration

#### Google TPU (`src/device/readers/google_tpu.rs`, `src/device/readers/tpu_grpc.rs`)
- Multi-channel discovery: Sysfs, VFIO, and Environment Variables (for TPU VMs)
- Dual-mode metrics collection:
  - **Native gRPC**: Direct connection to libtpu metrics server (localhost:8431) when workload is running
  - **CLI Fallback**: Background polling of `tpu-info` utility when gRPC is unavailable
- Supports HLO metrics: Queue size and detailed execution timing (percentiles)
- Integrated with standard NPU/GPU metrics for unified Prometheus export
- Memory and duty cycle monitoring across TPU generations (v2-v7/Ironwood)

### CPU Reader Implementations

#### Linux
- `/proc` filesystem parsing
- `lscpu` integration for topology
- Per-core utilization tracking

#### macOS
- `system_profiler` for hardware info
- `sysctl` for runtime metrics
- P/E-core detection for Apple Silicon

#### Windows (`src/device/cpu_windows.rs`, `src/device/windows_temp/`)
- WMI for processor information (MaxClockSpeed, cache sizes, socket count)
- Thread-local WMI connections for efficiency
- Temperature monitoring via fallback chain:
  1. **MSAcpi_ThermalZoneTemperature**: Standard ACPI thermal zones (root\WMI namespace)
  2. **AMD Ryzen Master SDK**: FFI integration with `AMDRyzenMasterMonitoringDLL.dll`
  3. **Intel WMI**: Intel-specific thermal zones (root\Intel namespace)
  4. **LibreHardwareMonitor**: Third-party WMI (root\LibreHardwareMonitor namespace)
- Graceful fallback: Returns `None` silently when all sources unavailable
- Availability caching: OnceCell/RwLock pattern to avoid repeated failed queries
- No error spam: WBEM_E_NOT_FOUND (0x8004100C) handled silently

### Conditional Compilation

```rust
#[cfg(target_os = "macos")]
mod apple_silicon;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod nvidia;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod nvidia_jetson;
```

## Networking and Remote Monitoring

### Connection Management

#### Connection Pool Configuration
- **Pool Size**: 200 idle connections per host
- **Keep-Alive**: TCP keepalive for persistent connections
- **Timeout**: 5-second request timeout
- **Concurrency**: Limited to 64 simultaneous connections

#### Resilience Features

1. **Retry Logic**
   - 3 attempts with exponential backoff
   - Delays: 50ms → 100ms → 150ms
   - Automatic failure recovery

2. **Connection Staggering**
   - 500ms delays for 100+ nodes
   - Prevents overwhelming system listen queues
   - Respects `kern.ipc.somaxconn` limits

3. **Adaptive Behavior**
   - Update intervals: 2-6 seconds based on cluster size
   - Dynamic timeout adjustments
   - Graceful degradation on failures

### Security Measures

1. **SSRF Prevention**
   - URL validation blocks internal services
   - Private IP detection and warnings
   - Path/query sanitization

2. **Authentication**
   - Bearer token support via `ALL_SMI_AUTH_TOKEN`
   - Automatic header injection
   - Secure credential handling

3. **Rate Limiting**
   - 10 requests/second per host
   - Sliding window implementation
   - Automatic cleanup of expired entries

## Terminal UI Implementation

### Rendering Architecture

#### Direct Terminal Manipulation
- Uses `crossterm` for low-level control
- No heavyweight TUI framework dependencies
- Custom widget system for flexibility

#### Rendering Strategy

1. **Differential Rendering**
   - Only updates changed screen areas
   - Reduces flicker and CPU usage
   - Maintains responsive interface

2. **Double Buffering**
   - Complete UI built in memory
   - Atomic terminal update
   - Eliminates visual artifacts

3. **Layout System**
   - Dynamic space allocation
   - Responsive to terminal resize
   - Content-aware sizing

### UI Components

#### Dashboard View
- Cluster-wide statistics
- Sparkline history graphs
- Real-time metric updates
- Color-coded health indicators

#### Process View
- Scrollable process table
- Sort by resource usage
- Highlighted GPU processes
- Memory usage breakdown

#### Storage View
- Disk usage visualization
- Mount point organization
- Filesystem type display
- Available space warnings

### Performance Optimizations

- **Adaptive Frame Rates**: Based on terminal size and node count
- **Content Calculations**: Account for UI chrome to prevent scrolling
- **Braille Characters**: Compact data visualization in limited space
- **Lazy Rendering**: Only visible content is processed

## Performance and Concurrency

### Async Architecture

#### Task Organization
- **Data Collection Tasks**: Run in background with tokio::spawn
- **UI Task**: Main thread handles terminal events
- **Network Tasks**: Concurrent remote fetching
- **State Updates**: Quick mutex locks for minimal contention

#### Caching Strategies

1. **Hardware Info Caching**
   - Static system information cached
   - Refresh only on significant changes
   - Reduces system call overhead

2. **Connection Pooling**
   - HTTP connections reused
   - DNS results cached
   - SSL session resumption

3. **Template Responses**
   - Mock server uses pre-allocated templates
   - 16KB response buffers
   - Minimal allocation during requests

### Concurrency Controls

1. **Semaphore Limiting**
   ```rust
   let semaphore = Arc::new(Semaphore::new(64));
   ```
   - Prevents resource exhaustion
   - Fair scheduling of requests
   - Backpressure handling

2. **Adaptive Intervals**
   ```rust
   fn adaptive_interval(node_count: usize) -> u64 {
       match node_count {
           0..=10 => 2,
           11..=50 => 3,
           51..=100 => 4,
           _ => 6,
       }
   }
   ```

## Error Handling and Recovery

### Error Type Hierarchy

1. **Application Errors**
   - Custom `AppError` enum (planned)
   - Domain-specific error types
   - Structured error information

2. **Library Errors**
   - `anyhow::Error` for general errors
   - `thiserror` for custom derives
   - Automatic `From` implementations

### Recovery Strategies

1. **Graceful Degradation**
   - Failed metrics don't crash app
   - Missing data shows as "N/A"
   - Partial results displayed

2. **Retry Mechanisms**
   - Network operations retry automatically
   - Exponential backoff prevents storms
   - Maximum retry limits

3. **Fallback Paths**
   - NVIDIA: NVML → nvidia-smi
   - Network: Primary → backup endpoints
   - UI: Full → simplified display

### Resource Cleanup

```rust
impl Drop for TerminalState {
    fn drop(&mut self) {
        // Restore terminal on panic
        disable_raw_mode();
        execute!(stdout(), LeaveAlternateScreen);
    }
}
```

## Security Architecture

### Privilege Management

1. **Minimal Privileges**
   - Sudo only for `powermetrics` on macOS
   - Drops privileges after initialization
   - No root required on Linux with proper groups

2. **Process Isolation**
   - Child processes in separate groups
   - Clean termination on shutdown
   - No orphaned processes

### Input Validation

1. **Metric Parsing**
   - Bounds checking on all values
   - Regex size limits (10MB DFA)
   - Input truncation at 32KB

2. **URL Validation**
   - Scheme restrictions (http/https only)
   - Port range validation
   - Path traversal prevention

3. **Command Safety**
   - No shell interpretation
   - Argument validation
   - Timeout protection

### Container Awareness

- Detects containerized environments
- Adjusts behavior appropriately
- Respects resource limits
- Handles namespace isolation

## Configuration Management

### Static Configuration

```rust
pub const DEFAULT_UPDATE_INTERVAL: u64 = 3;
pub const MAX_CONCURRENT_CONNECTIONS: usize = 64;
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_HISTORY_SIZE: usize = 60;
```

### Dynamic Configuration

1. **Environment Variables**
   ```bash
   ALL_SMI_AUTH_TOKEN=secret
   SUPPRESS_LOCALHOST_WARNING=1
   ALL_SMI_MAX_CONNECTIONS=128
   ```

2. **Runtime Adjustments**
   - User-configurable update intervals
   - Display option toggles
   - Sort order preferences

3. **Platform Detection**
   - Automatic hardware detection
   - OS-specific optimizations
   - Driver availability checks

## Build System and Features

### Feature Flags

```toml
[features]
default = []
mock = []  # Enables mock server binary
```

### Binary Targets

1. **Main Application**
   ```toml
   [[bin]]
   name = "all-smi"
   path = "src/main.rs"
   ```

2. **Mock Server**
   ```toml
   [[bin]]
   name = "all-smi-mock-server"
   required-features = ["mock"]
   ```

### Platform Dependencies

```toml
[target.'cfg(target_os = "linux")'.dependencies]
nvml-wrapper = "0.10.0"

[target.'cfg(target_os = "macos")'.dependencies]
metal = "0.29"
objc = "0.2"
```

### Build Optimizations

- **Release Profile**
  ```toml
  [profile.release]
  lto = true
  codegen-units = 1
  opt-level = 3
  ```

- **Static Linking**
  - Vendored OpenSSL for musl
  - Self-contained binaries
  - Cross-compilation support

## Strategy Pattern for Data Collection

### Implementation Details (PR #53)

The Strategy pattern refactoring introduced a clean separation between data collection strategies:

#### Problem Statement
Previously, data collection logic for local and remote sources was tightly coupled within the `DataCollector`, making it difficult to:
- Test individual collection strategies
- Add new data sources
- Handle different collection configurations
- Maintain separation of concerns

### Solution: Strategy Pattern Implementation

#### Core Components

##### 1. DataCollectionStrategy Trait
```rust
#[async_trait]
pub trait DataCollectionStrategy: Send + Sync {
    async fn collect(&self, config: &CollectionConfig) -> CollectionResult;
    async fn update_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        data: CollectionData,
        config: &CollectionConfig
    );
    fn strategy_type(&self) -> &str;
    async fn is_ready(&self) -> bool;
}
```

The trait defines a unified interface for all data collection strategies, enabling:
- Polymorphic behavior through dynamic dispatch
- Consistent error handling via `CollectionResult`
- Flexible state updates
- Strategy identification for logging

##### 2. Concrete Strategy Implementations

###### LocalCollector
- Collects metrics directly from the host system
- Manages platform-specific readers (GPU, CPU, Memory)
- Handles reader initialization with timeout protection
- Implements lazy initialization pattern
- Features:
  - Direct hardware access via platform APIs
  - Process information collection
  - Storage metrics gathering
  - System information aggregation

###### RemoteCollector
- Fetches metrics from remote All-SMI instances via HTTP
- Parses Prometheus format metrics
- Implements connection pooling and rate limiting
- Features:
  - Concurrent connection management (semaphore-based)
  - Optimized regex parsing with DFA size limits
  - Connection staggering for high-scale deployments
  - Deduplication of storage information

##### 3. DataAggregator
- Centralized aggregation logic
- Maintains utilization history
- Calculates moving averages
- Updates trend indicators
- Responsibilities:
  - CPU utilization history tracking
  - GPU metrics aggregation
  - Memory usage trends
  - Cross-node statistics

##### 4. CollectionConfig
```rust
pub struct CollectionConfig {
    pub interval: u64,
    pub first_iteration: bool,
    pub hosts: Vec<String>,
}
```

Centralized configuration management:
- Adaptive interval calculation based on node count
- First iteration special handling
- Host list management for remote monitoring

##### 5. CollectionError
```rust
pub enum CollectionError {
    ConnectionError(String),
    ParseError(String),
    IoError(#[from] std::io::Error),
    Other(String),
}
```

Unified error handling across all strategies with automatic conversion from `std::io::Error`.

### Benefits of the Strategy Pattern

1. **Separation of Concerns**: Each strategy encapsulates its specific collection logic
2. **Open/Closed Principle**: New collection strategies can be added without modifying existing code
3. **Testability**: Each strategy can be unit tested independently
4. **Flexibility**: Strategies can be swapped at runtime based on configuration
5. **Code Reusability**: Common logic is extracted to the aggregator
6. **Maintainability**: Clearer code organization with defined responsibilities

### Usage Pattern

```rust
// Local mode
let collector = LocalCollector::new();
let data = collector.collect(&config).await?;
collector.update_state(app_state, data, &config).await;

// Remote mode
let collector = RemoteCollectorBuilder::new()
    .with_max_connections(64)
    .with_hosts(hosts)
    .build();
let data = collector.collect(&config).await?;
collector.update_state(app_state, data, &config).await;
```

## Security Enhancements (PR #53)

### 1. SSRF Prevention
- **URL Validation**: Comprehensive URL validation in `NetworkClient`
  - Scheme validation (only http/https)
  - Private IP detection and warnings
  - Path/query/fragment sanitization
  - Port range validation
- **Localhost Warning Suppression**: Environment variable control for development

### 2. ReDoS Protection
- **Regex Limits**: Size and complexity limits using `RegexBuilder`
  - 10MB DFA size limit
  - Optimized patterns to prevent quadratic complexity
- **Input Size Limits**: 32KB maximum for regex processing

### 3. Command Injection Prevention
- **PowerMetrics Validation**:
  - Sampler name restricted to alphanumeric + underscore
  - Nice value range validation
  - Interval range validation
  - Safe defaults on validation failure

### 4. Authentication Support
- **Bearer Token**: Via `ALL_SMI_AUTH_TOKEN` environment variable
- **Automatic Header Injection**: All remote requests include auth headers

### 5. Rate Limiting
- **Per-Host Limits**: 10 requests/second sliding window
- **Automatic Cleanup**: Expired rate limit entries removed
- **Graceful Degradation**: Returns error when limit exceeded

### 6. Memory Exhaustion Prevention
- **HashMap Limits**: Maximum 256 devices per type
- **Input Truncation**: 10MB maximum with automatic truncation
- **Pre-allocated Capacity**: Efficient memory usage

### 7. Resource Leak Prevention
- **Panic Hooks**: Cleanup subprocesses on panic
- **Catch Unwind**: Reader threads wrapped for safety
- **Process Group Management**: Ensures proper parent-child cleanup

## Performance Optimizations (PR #53)

### 1. Parsing Optimizations
- **Label Parsing**:
  - Removed intermediate vector allocations
  - Direct string slicing
  - Single-pass validation
  - O(n) complexity instead of O(n²)

### 2. Concurrency Control
- **Mutex Timeouts**:
  - 5-second timeout on initialization locks
  - 2-second timeout on reader/writer locks
  - Graceful degradation with warnings

### 3. Memory Efficiency
- **Arc Usage**: Explicit `Arc::clone` for clarity
- **String Operations**: Pre-allocated capacity (8KB)
- **std::mem::take**: Avoid unnecessary clones

### 4. Connection Management
- **Staggered Connections**: 500ms spread for 100+ nodes
- **Connection Pool**: 200 idle connections per host
- **TCP Keepalive**: Maintains persistent connections
- **Retry Logic**: 3 attempts with exponential backoff

### 5. Input Validation
- **Hostfile Limits**:
  - Path traversal prevention
  - 10MB file size limit
  - 1000 host maximum
  - ASCII validation

## Module Organization

### `/src/view/data_collection/`
```
mod.rs              # Module exports and re-exports
strategy.rs         # Trait definition and common types
local_collector.rs  # Local data collection implementation
remote_collector.rs # Remote data collection implementation
aggregator.rs       # Data aggregation logic
```

### Integration Points

1. **DataCollector** (`/src/view/data_collector.rs`)
   - Orchestrates strategy usage
   - Manages collection loops
   - Handles mode selection (local/remote)

2. **AppState** (`/src/app_state.rs`)
   - Shared state container
   - Mutex-protected for concurrent access
   - Updated by strategies

3. **NetworkClient** (`/src/network/client.rs`)
   - HTTP client with security validations
   - Connection pooling
   - Rate limiting implementation

4. **MetricsParser** (`/src/network/metrics_parser.rs`)
   - Prometheus format parsing
   - Size-limited processing
   - Efficient regex matching

## Configuration and Environment

### Environment Variables
- `ALL_SMI_AUTH_TOKEN`: Bearer token for remote authentication
- `SUPPRESS_LOCALHOST_WARNING`: Suppress localhost connection warnings
- `ALL_SMI_MAX_CONNECTIONS`: Override max concurrent connections

### Adaptive Behavior
- **Connection Limits**: Based on system file descriptor limits
- **Update Intervals**: 2-6 seconds based on node count
- **Connection Concurrency**: Limited to 64 to respect system limits

## Testing Considerations

### Unit Testing
- Each strategy can be tested independently
- Mock implementations of the trait for testing
- Isolated aggregator testing

### Integration Testing
- Mock server for remote collector testing
- Platform-specific reader testing
- End-to-end collection pipeline testing

### Performance Testing
- High-scale connection testing (128+ nodes)
- Memory usage under load
- Parsing performance with large inputs

## Future Extensibility

### Potential New Strategies
1. **FileCollector**: Read metrics from files
2. **DatabaseCollector**: Query metrics from databases
3. **CloudCollector**: Integrate with cloud provider APIs
4. **HybridCollector**: Combine multiple strategies

### Enhancement Opportunities
1. **Caching Layer**: Add caching to reduce collection overhead
2. **Compression**: Compress remote data transfer
3. **Metrics Export**: Additional export formats beyond Prometheus
4. **Plugin System**: Dynamic strategy loading

### Integration with Overall Architecture

The Strategy pattern implementation integrates seamlessly with the existing architecture:
- **Async Compatibility**: All strategies implement async traits
- **State Management**: Unified AppState updates across strategies
- **Error Handling**: Consistent error propagation
- **Performance**: Optimized for high-scale deployments

## Testing Strategy

### Unit Testing

1. **Module Tests**
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;

       #[tokio::test]
       async fn test_local_collector() {
           // Test implementation
       }
   }
   ```

2. **Property Testing**
   - Input fuzzing for parsers
   - Boundary value testing
   - Error path validation

### Integration Testing

1. **Hardware Integration**
   - Mock implementations for CI
   - Real hardware tests locally
   - Platform-specific test suites

2. **Network Integration**
   - Mock server for remote testing
   - Connection failure scenarios
   - High-scale simulations

### End-to-End Testing

1. **Shell Scripts**
   ```bash
   test_powermetrics_cleanup.sh
   test_high_scale.sh
   test_error_recovery.sh
   ```

2. **Performance Testing**
   - Load testing with mock server
   - Memory leak detection
   - CPU profiling

## Dependencies Overview

### Core Runtime
- **tokio**: Async runtime with full features
- **clap**: Modern CLI parsing with derives
- **anyhow**: Flexible error handling

### Networking
- **axum**: Lightweight async web framework
- **reqwest**: Feature-rich HTTP client
- **hyper**: Low-level HTTP implementation

### Hardware Interface
- **nvml-wrapper**: NVIDIA GPU management
- **sysinfo**: Cross-platform system info
- **metal/objc**: macOS Metal framework

### Data Processing
- **serde**: High-performance serialization
- **regex**: Compiled pattern matching
- **chrono**: Comprehensive datetime handling

### UI/Terminal
- **crossterm**: Cross-platform terminal control
- **unicode-width**: Accurate text rendering

## Code Quality Patterns

### Macro System

The parsing macro DSL reduces boilerplate:

```rust
macro_rules! parse_metric {
    ($line:expr, $pattern:expr, $unit:expr) => {
        // Sophisticated parsing implementation
    };
}
```

### Generic Programming

1. **Type-Safe Parsing**
   ```rust
   fn parse_number<T: FromStr>(s: &str) -> Option<T> {
       s.trim().parse().ok()
   }
   ```

2. **Associated Types**
   ```rust
   trait Collector {
       type Data;
       type Error;
   }
   ```

### Memory Management

1. **RAII Patterns**
   - Automatic cleanup via Drop
   - Scoped guards for resources
   - No manual memory management

2. **Smart Pointers**
   - `Arc<Mutex<T>>` for shared state
   - `Box<dyn Trait>` for polymorphism
   - `Rc` avoided due to async requirements

## Future Roadmap

### Planned Enhancements

1. **Additional Platforms**
   - Intel Arc GPU via oneAPI
   - Qualcomm NPU support

2. **Extended Metrics**
   - Network bandwidth monitoring
   - PCIe throughput tracking
   - Tensor core utilization

3. **Advanced Features**
   - Historical data persistence
   - Alerting and thresholds
   - Cluster management UI
   - REST API for configuration

4. **Performance Improvements**
   - Zero-copy parsing
   - SIMD optimizations
   - Custom allocators

### Architecture Evolution

1. **Plugin System**
   - Dynamic library loading
   - Custom metric collectors
   - Third-party integrations

2. **Distributed Architecture**
   - Peer-to-peer metric sharing
   - Consensus-based aggregation
   - Fault-tolerant clustering

3. **Cloud Native**
   - Kubernetes operator
   - Helm charts
   - Service mesh integration
   - Cloud provider APIs

## Conclusion

All-SMI represents a sophisticated example of modern systems programming in Rust. The architecture successfully balances:

- **Performance**: Efficient resource usage and minimal overhead
- **Reliability**: Robust error handling and recovery
- **Maintainability**: Clean module boundaries and clear abstractions
- **Extensibility**: Plugin architecture and trait-based design
- **Security**: Comprehensive validation and privilege management
- **User Experience**: Responsive UI and intuitive interactions

The recent Strategy pattern refactoring (PR #53) exemplifies the project's commitment to continuous improvement, establishing patterns that will serve the project well as it scales to support new platforms and use cases.

The codebase demonstrates best practices in:
- Async Rust programming
- Cross-platform systems development
- Security-conscious design
- Performance optimization
- User interface implementation

This architecture provides a solid foundation for future enhancements while maintaining the flexibility to adapt to evolving requirements in the GPU monitoring space.