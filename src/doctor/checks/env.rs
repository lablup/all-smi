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

//! `env.*` checks — summarise which of the relevant environment-variable
//! families (ALL_SMI_*, CUDA_*, ROCR_*, TPU_*, HL_*) are present.

use crate::doctor::types::{Check, CheckCtx, CheckResult, Severity};

static CHECKS: &[&Check] = &[&ALL_SMI, &CUDA, &ROCR, &TPU, &HL];

pub fn checks() -> &'static [&'static Check] {
    CHECKS
}

static ALL_SMI: Check = Check {
    id: "env.all_smi",
    title: "ALL_SMI_* environment variables",
    severity_on_fail: Severity::Info,
    run: check_all_smi,
};

static CUDA: Check = Check {
    id: "env.cuda",
    title: "CUDA_* environment variables",
    severity_on_fail: Severity::Info,
    run: check_cuda,
};

static ROCR: Check = Check {
    id: "env.rocr",
    title: "ROCR_* / HIP_* environment variables",
    severity_on_fail: Severity::Info,
    run: check_rocr,
};

static TPU: Check = Check {
    id: "env.tpu",
    title: "TPU_* environment variables",
    severity_on_fail: Severity::Info,
    run: check_tpu,
};

static HL: Check = Check {
    id: "env.hl",
    title: "HL_* / HABANA_* environment variables",
    severity_on_fail: Severity::Info,
    run: check_hl,
};

fn collect_prefix(prefix: &str) -> Vec<String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(k, _)| k)
        .collect()
}

fn describe(names: &[String]) -> String {
    if names.is_empty() {
        "none set".to_string()
    } else {
        format!("{} set: {}", names.len(), names.join(", "))
    }
}

fn check_all_smi(_ctx: &CheckCtx) -> CheckResult {
    CheckResult::Pass(describe(&collect_prefix("ALL_SMI_")))
}

fn check_cuda(_ctx: &CheckCtx) -> CheckResult {
    // Include both CUDA_ and NVIDIA_ prefixes — the container runtime
    // injects NVIDIA_VISIBLE_DEVICES, which is relevant alongside CUDA.
    let mut names = collect_prefix("CUDA_");
    names.extend(collect_prefix("NVIDIA_"));
    names.sort();
    names.dedup();
    CheckResult::Pass(describe(&names))
}

fn check_rocr(_ctx: &CheckCtx) -> CheckResult {
    let mut names = collect_prefix("ROCR_");
    names.extend(collect_prefix("HIP_"));
    names.extend(collect_prefix("HSA_"));
    names.sort();
    names.dedup();
    CheckResult::Pass(describe(&names))
}

fn check_tpu(_ctx: &CheckCtx) -> CheckResult {
    let mut names = collect_prefix("TPU_");
    names.extend(collect_prefix("CLOUD_TPU_"));
    names.sort();
    names.dedup();
    CheckResult::Pass(describe(&names))
}

fn check_hl(_ctx: &CheckCtx) -> CheckResult {
    let mut names = collect_prefix("HL_");
    names.extend(collect_prefix("HABANA_"));
    names.sort();
    names.dedup();
    CheckResult::Pass(describe(&names))
}
