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

pub mod chassis_renderer;
pub mod cpu_renderer;
pub mod gpu_renderer;
pub mod memory_renderer;
pub mod storage_renderer;
pub mod vgpu_renderer;
pub mod widgets;

// Re-export the main rendering functions for backward compatibility
pub use chassis_renderer::print_chassis_info;
pub use cpu_renderer::print_cpu_info;
pub use gpu_renderer::print_gpu_info;
pub use memory_renderer::print_memory_info;
pub use storage_renderer::print_storage_info;
pub use vgpu_renderer::print_vgpu_section;

// Re-export renderer structs if needed in the future
#[allow(unused_imports)]
pub use chassis_renderer::ChassisRenderer;
#[allow(unused_imports)]
pub use cpu_renderer::CpuRenderer;
#[allow(unused_imports)]
pub use gpu_renderer::GpuRenderer;
#[allow(unused_imports)]
pub use memory_renderer::MemoryRenderer;
#[allow(unused_imports)]
pub use storage_renderer::StorageRenderer;
