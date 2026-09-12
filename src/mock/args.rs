//! Command-line argument parsing for the mock server

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

use crate::mock::constants::DEFAULT_NVIDIA_GPU_NAME;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "High-performance GPU metrics mock server", long_about = None)]
pub struct Args {
    #[arg(long, help = "Port range, e.g., 10001-10010 or 10001")]
    pub port_range: Option<String>,

    #[arg(long, default_value = DEFAULT_NVIDIA_GPU_NAME, help = "GPU name")]
    pub gpu_name: String,

    #[arg(
        long,
        default_value = "nvidia",
        help = "Platform type: nvidia, apple, jetson, intel, amd, amdgpu, tenstorrent, rebellions, furiosa, gaudi, neuron"
    )]
    pub platform: String,

    #[arg(
        short,
        long,
        default_value = "hosts.csv",
        help = "Output CSV file name"
    )]
    pub o: String,

    #[arg(
        long,
        default_value_t = 0,
        help = "Number of nodes to simulate random failures (0 = no failures)"
    )]
    pub failure_nodes: u32,

    #[arg(
        long,
        default_value_t = 1,
        help = "Starting index for node naming (e.g., 51 for node-0051)"
    )]
    pub start_index: u32,
}
