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

// Re-export all the renderer functions from their respective modules
pub use crate::ui::chrome::{print_function_keys, print_loading_indicator};
pub use crate::ui::process_renderer::print_process_info;
pub use crate::ui::renderers::{
    print_chassis_info, print_cpu_info, print_gpu_info, print_memory_info, print_mig_section,
    print_storage_info, print_vgpu_section,
};
