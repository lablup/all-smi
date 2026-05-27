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

use super::*;

#[test]
fn sysman_structure_type_constants_match_spec() {
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_FAN_PROPERTIES, 0x0000_0007);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_FREQ_PROPERTIES, 0x0000_0009);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_MEM_PROPERTIES, 0x0000_000b);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_TEMP_PROPERTIES, 0x0000_0014);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_FREQ_STATE, 0x0000_001b);
    assert_eq!(ffi::ZES_STRUCTURE_TYPE_MEM_STATE, 0x0000_001e);
}

#[test]
fn sysman_enum_values_match_spec() {
    assert_eq!(ffi::ZES_TEMP_SENSORS_GLOBAL, 0);
    assert_eq!(ffi::ZES_TEMP_SENSORS_GPU, 1);
    assert_eq!(ffi::ZES_TEMP_SENSORS_MEMORY, 2);
    assert_eq!(ffi::ZES_TEMP_SENSORS_GPU_BOARD, 6);
    assert_eq!(ffi::ZES_MEM_LOC_SYSTEM, 0);
    assert_eq!(ffi::ZES_MEM_LOC_DEVICE, 1);
    assert_eq!(ffi::ZES_FREQ_DOMAIN_GPU, 0);
    assert_eq!(ffi::ZES_FREQ_DOMAIN_MEMORY, 1);
    assert_eq!(ffi::ZES_FREQ_DOMAIN_MEDIA, 2);
    assert_eq!(ffi::ZES_FAN_SPEED_UNITS_RPM, 0);
    assert_eq!(ffi::ZES_FAN_SPEED_UNITS_PERCENT, 1);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn sysman_point_struct_sizes_match_spec() {
    assert_eq!(std::mem::size_of::<ffi::zes_temp_properties_t>(), 48);
    assert_eq!(std::mem::size_of::<ffi::zes_mem_properties_t>(), 48);
    assert_eq!(std::mem::size_of::<ffi::zes_mem_state_t>(), 40);
    assert_eq!(std::mem::size_of::<ffi::zes_freq_properties_t>(), 48);
    assert_eq!(std::mem::size_of::<ffi::zes_freq_state_t>(), 64);
    assert_eq!(std::mem::size_of::<ffi::zes_fan_properties_t>(), 48);
}
