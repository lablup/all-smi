/*
 * Copyright 2025 Lablup Inc. and Jeongkyu Shin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/*
 * A synthetic Level Zero loader, built and used only by CI.
 *
 * ## Why it exists
 *
 * Every runner this project has lacks an Intel GPU, so `zeInit` has no
 * driver to return and everything past `try_load_library` goes unexercised:
 * device enumeration, the count-then-buffer idiom, the BDF mapping, the
 * delta arithmetic, and the struct layouts the driver writes into. This
 * library plays the driver's part so that code runs on an ordinary runner.
 *
 * ## What it verifies
 *
 * It is compiled against the **vendor headers**, not against all-smi's own
 * transcription of them. That is the entire point. The existing size
 * assertions in `tests/ffi_layout.rs` compare our struct to our own reading
 * of the spec, which catches a mis-sized field but not two same-typed
 * fields in the wrong order, nor a wrong offset that preserves the total,
 * nor a spec value transcribed wrong in both places. Here the vendor's own
 * struct definition is filled in on the C side and read back through
 * all-smi's `#[repr(C)]` type on the Rust side, so a value arriving intact
 * proves the two layouts agree field by field.
 *
 * Types verified this way, all in `src/device/readers/intel_gpu_level_zero`:
 *   ffi.rs         zes_pci_address_t, zes_pci_speed_t, zes_pci_properties_t,
 *                  zes_engine_properties_t, zes_engine_stats_t,
 *                  zes_power_energy_counter_t
 *   ffi/sysman.rs  zes_temp_properties_t, zes_mem_properties_t,
 *                  zes_mem_state_t, zes_freq_properties_t,
 *                  zes_freq_state_t, zes_fan_properties_t
 *
 * ## What it does NOT verify
 *
 * Real driver behaviour, plausible value ranges, error paths a real driver
 * would take, Windows and `ze_loader.dll` (out of scope, see the issue),
 * and any Sysman domain not implemented below. A green run here means the
 * plumbing and the layouts are right, not that an Intel GPU would report
 * anything in particular.
 *
 * ## Determinism
 *
 * No clock reads and no randomness. Counters advance by a fixed step per
 * call so the derived percentages and watts are exact constants that the
 * test asserts on directly, never ranges.
 */

#include <level_zero/ze_api.h>
#include <level_zero/zes_api.h>

#include <stdatomic.h>
#include <stdint.h>
#include <string.h>

/* ------------------------------------------------------------------ *
 * Handles
 *
 * Every handle is the address of a distinct static object, so a handle
 * mixed up between calls dereferences to visibly wrong data rather than
 * to a plausible neighbour.
 * ------------------------------------------------------------------ */

static int stub_driver;

/* Two devices at addresses that exercise a non-trivial bus byte and a
 * sort order: 0xaf sorts after 0x03 numerically and as a string. */
static int stub_device_a;
static int stub_device_b;

static int stub_engine_compute;
static int stub_engine_render;
static int stub_power;
static int stub_temp;
static int stub_mem;
static int stub_freq;
static int stub_fan;

/* Device B owns one render engine and deliberately rejects power-domain
 * enumeration. It exists to prove that a second device is enumerated and
 * sorted, to exercise the count edge cases below, and to verify that one
 * failing Sysman family does not suppress the others. */

/* ------------------------------------------------------------------ *
 * Fixed per-call steps
 *
 * The backend derives engine busy from delta(activeTime)/delta(timestamp)
 * and power from delta(energy in microjoules)/delta(timestamp in
 * microseconds), which is microjoules per microsecond, that is watts. With
 * the steps below the second refresh must produce exactly 25.00% compute,
 * 10.00% render, and 45.00 W.
 * ------------------------------------------------------------------ */

#define STUB_TICK_US            1000000ULL
#define STUB_COMPUTE_ACTIVE_US   250000ULL   /* 25.00% of a tick */
#define STUB_RENDER_ACTIVE_US    100000ULL   /* 10.00% of a tick */
#define STUB_ENERGY_UJ         45000000ULL   /* 45.00 W over a tick */

/* Rust's test harness runs independent tests concurrently. These counters
 * are shared by every test through the one process-wide stub library, so
 * plain increments would be a C data race. Relaxed atomics are sufficient:
 * callers need a unique monotonically increasing sample number, not an
 * ordering relationship with any other state. */
static _Atomic uint64_t compute_calls;
static _Atomic uint64_t render_calls;
static _Atomic uint64_t power_calls;

static uint64_t next_call(_Atomic uint64_t *counter) {
    return atomic_fetch_add_explicit(counter, 1, memory_order_relaxed) + 1;
}

/* Point-in-time values. Each is unique within its struct so a field read
 * at the wrong offset yields an obviously wrong number. */
#define STUB_TEMP_C            61.0
#define STUB_MEM_SIZE      12884901888ULL   /* 12 GiB */
#define STUB_MEM_FREE       4294967296ULL   /*  4 GiB, so 8 GiB used */
#define STUB_FREQ_ACTUAL_MHZ    2100.0
#define STUB_FREQ_MIN_MHZ        300.0
#define STUB_FREQ_MAX_MHZ       2400.0
#define STUB_FAN_RPM            1800

/* ------------------------------------------------------------------ *
 * Count-then-buffer helper
 *
 * The real API contract, which `enumerate_devices` and every `populate_*`
 * helper depend on: a non-null count pointer with a null buffer means
 * "report how many", and a second call fills that many.
 * ------------------------------------------------------------------ */
static ze_result_t stub_enumerate(uint32_t *pCount, void **buffer,
                                  void **items, uint32_t available) {
    if (pCount == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (buffer == NULL) {
        *pCount = available;
        return ZE_RESULT_SUCCESS;
    }
    uint32_t n = *pCount < available ? *pCount : available;
    for (uint32_t i = 0; i < n; i++) {
        buffer[i] = items[i];
    }
    *pCount = n;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Core
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL zeInit(ze_init_flags_t flags) {
    (void)flags;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL zesInit(zes_init_flags_t flags) {
    (void)flags;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL zeDriverGet(uint32_t *pCount,
                                                ze_driver_handle_t *phDrivers) {
    void *items[] = {&stub_driver};
    return stub_enumerate(pCount, (void **)phDrivers, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL zeDeviceGet(ze_driver_handle_t hDriver,
                                                uint32_t *pCount,
                                                ze_device_handle_t *phDevices) {
    if (hDriver != (ze_driver_handle_t)&stub_driver) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    void *items[] = {&stub_device_a, &stub_device_b};
    return stub_enumerate(pCount, (void **)phDevices, items, 2);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDevicePciGetProperties(zes_device_handle_t hDevice,
                          zes_pci_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_PCI_PROPERTIES;
    pProperties->pNext = NULL;

    if (hDevice == (zes_device_handle_t)&stub_device_a) {
        pProperties->address.domain = 0x0000;
        pProperties->address.bus = 0x03;
        pProperties->address.device = 0x00;
        pProperties->address.function = 0x0;
    } else if (hDevice == (zes_device_handle_t)&stub_device_b) {
        pProperties->address.domain = 0x0000;
        pProperties->address.bus = 0xaf;
        pProperties->address.device = 0x00;
        pProperties->address.function = 0x0;
    } else {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }

    /* Filled so a wrong offset in the trailing fields is observable as a
     * changed address rather than as silent garbage. */
    pProperties->maxSpeed.gen = 4;
    pProperties->maxSpeed.width = 16;
    pProperties->maxSpeed.maxBandwidth = 31504000000LL;
    pProperties->haveBandwidthCounters = 1;
    pProperties->havePacketCounters = 0;
    pProperties->haveReplayCounters = 1;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Engines
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumEngineGroups(zes_device_handle_t hDevice, uint32_t *pCount,
                          zes_engine_handle_t *phEngine) {
    if (hDevice == (zes_device_handle_t)&stub_device_b) {
        /* Edge case: report more than MAX_L0_HANDLES so `cap_handle_count`
         * has something to clamp, then fill fewer than requested so the
         * `truncate` after the fill call has something to trim. */
        if (pCount == NULL) {
            return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
        }
        if (phEngine == NULL) {
            *pCount = 4096;
            return ZE_RESULT_SUCCESS;
        }
        phEngine[0] = (zes_engine_handle_t)&stub_engine_render;
        *pCount = 1;
        return ZE_RESULT_SUCCESS;
    }
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    void *items[] = {&stub_engine_compute, &stub_engine_render};
    return stub_enumerate(pCount, (void **)phEngine, items, 2);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesEngineGetProperties(zes_engine_handle_t hEngine,
                       zes_engine_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES;
    pProperties->pNext = NULL;
    pProperties->onSubdevice = 0;
    pProperties->subdeviceId = 0;
    if (hEngine == (zes_engine_handle_t)&stub_engine_compute) {
        pProperties->type = ZES_ENGINE_GROUP_COMPUTE_SINGLE;
    } else if (hEngine == (zes_engine_handle_t)&stub_engine_render) {
        pProperties->type = ZES_ENGINE_GROUP_RENDER_SINGLE;
    } else {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesEngineGetActivity(zes_engine_handle_t hEngine, zes_engine_stats_t *pStats) {
    if (pStats == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    memset(pStats, 0, sizeof(*pStats));
    if (hEngine == (zes_engine_handle_t)&stub_engine_compute) {
        uint64_t call = next_call(&compute_calls);
        pStats->activeTime = call * STUB_COMPUTE_ACTIVE_US;
        pStats->timestamp = call * STUB_TICK_US;
    } else if (hEngine == (zes_engine_handle_t)&stub_engine_render) {
        uint64_t call = next_call(&render_calls);
        pStats->activeTime = call * STUB_RENDER_ACTIVE_US;
        pStats->timestamp = call * STUB_TICK_US;
    } else {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Power
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumPowerDomains(zes_device_handle_t hDevice, uint32_t *pCount,
                          zes_pwr_handle_t *phPower) {
    if (hDevice == (zes_device_handle_t)&stub_device_b) {
        return ZE_RESULT_ERROR_UNSUPPORTED_FEATURE;
    }
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    void *items[] = {&stub_power};
    return stub_enumerate(pCount, (void **)phPower, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesPowerGetEnergyCounter(zes_pwr_handle_t hPower,
                         zes_power_energy_counter_t *pEnergy) {
    if (pEnergy == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hPower != (zes_pwr_handle_t)&stub_power) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pEnergy, 0, sizeof(*pEnergy));
    uint64_t call = next_call(&power_calls);
    pEnergy->energy = call * STUB_ENERGY_UJ;
    pEnergy->timestamp = call * STUB_TICK_US;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Temperature
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumTemperatureSensors(zes_device_handle_t hDevice, uint32_t *pCount,
                                zes_temp_handle_t *phTemperature) {
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        if (pCount != NULL && phTemperature == NULL) {
            *pCount = 0;
        }
        return ZE_RESULT_SUCCESS;
    }
    void *items[] = {&stub_temp};
    return stub_enumerate(pCount, (void **)phTemperature, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesTemperatureGetProperties(zes_temp_handle_t hTemperature,
                            zes_temp_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hTemperature != (zes_temp_handle_t)&stub_temp) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_TEMP_PROPERTIES;
    pProperties->pNext = NULL;
    pProperties->type = ZES_TEMP_SENSORS_GPU;
    pProperties->onSubdevice = 0;
    pProperties->subdeviceId = 0;
    pProperties->maxTemperature = 105.0;
    pProperties->isCriticalTempSupported = 1;
    pProperties->isThreshold1Supported = 0;
    pProperties->isThreshold2Supported = 0;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesTemperatureGetState(zes_temp_handle_t hTemperature, double *pTemperature) {
    if (pTemperature == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hTemperature != (zes_temp_handle_t)&stub_temp) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    *pTemperature = STUB_TEMP_C;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Memory
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumMemoryModules(zes_device_handle_t hDevice, uint32_t *pCount,
                           zes_mem_handle_t *phMemory) {
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        if (pCount != NULL && phMemory == NULL) {
            *pCount = 0;
        }
        return ZE_RESULT_SUCCESS;
    }
    void *items[] = {&stub_mem};
    return stub_enumerate(pCount, (void **)phMemory, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesMemoryGetProperties(zes_mem_handle_t hMemory,
                       zes_mem_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hMemory != (zes_mem_handle_t)&stub_mem) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_MEM_PROPERTIES;
    pProperties->pNext = NULL;
    pProperties->type = ZES_MEM_TYPE_HBM;
    pProperties->onSubdevice = 0;
    pProperties->subdeviceId = 0;
    pProperties->location = ZES_MEM_LOC_DEVICE;
    pProperties->physicalSize = STUB_MEM_SIZE;
    pProperties->busWidth = 256;
    pProperties->numChannels = 8;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesMemoryGetState(zes_mem_handle_t hMemory, zes_mem_state_t *pState) {
    if (pState == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hMemory != (zes_mem_handle_t)&stub_mem) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pState, 0, sizeof(*pState));
    pState->stype = ZES_STRUCTURE_TYPE_MEM_STATE;
    pState->pNext = NULL;
    pState->health = ZES_MEM_HEALTH_OK;
    pState->free = STUB_MEM_FREE;
    pState->size = STUB_MEM_SIZE;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Frequency
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumFrequencyDomains(zes_device_handle_t hDevice, uint32_t *pCount,
                              zes_freq_handle_t *phFrequency) {
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        if (pCount != NULL && phFrequency == NULL) {
            *pCount = 0;
        }
        return ZE_RESULT_SUCCESS;
    }
    void *items[] = {&stub_freq};
    return stub_enumerate(pCount, (void **)phFrequency, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesFrequencyGetProperties(zes_freq_handle_t hFrequency,
                          zes_freq_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hFrequency != (zes_freq_handle_t)&stub_freq) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_FREQ_PROPERTIES;
    pProperties->pNext = NULL;
    pProperties->type = ZES_FREQ_DOMAIN_GPU;
    pProperties->onSubdevice = 0;
    pProperties->subdeviceId = 0;
    pProperties->canControl = 1;
    pProperties->isThrottleEventSupported = 0;
    pProperties->min = STUB_FREQ_MIN_MHZ;
    pProperties->max = STUB_FREQ_MAX_MHZ;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesFrequencyGetState(zes_freq_handle_t hFrequency, zes_freq_state_t *pState) {
    if (pState == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hFrequency != (zes_freq_handle_t)&stub_freq) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pState, 0, sizeof(*pState));
    pState->stype = ZES_STRUCTURE_TYPE_FREQ_STATE;
    pState->pNext = NULL;
    /* Each field a different number: `actual` read at the offset of any
     * neighbour produces a visibly wrong frequency. */
    pState->currentVoltage = 1.05;
    pState->request = 2200.0;
    pState->tdp = 2300.0;
    pState->efficient = 1200.0;
    pState->actual = STUB_FREQ_ACTUAL_MHZ;
    pState->throttleReasons = 0;
    return ZE_RESULT_SUCCESS;
}

/* ------------------------------------------------------------------ *
 * Fan
 * ------------------------------------------------------------------ */

ZE_APIEXPORT ze_result_t ZE_APICALL
zesDeviceEnumFans(zes_device_handle_t hDevice, uint32_t *pCount,
                  zes_fan_handle_t *phFan) {
    if (hDevice != (zes_device_handle_t)&stub_device_a) {
        if (pCount != NULL && phFan == NULL) {
            *pCount = 0;
        }
        return ZE_RESULT_SUCCESS;
    }
    void *items[] = {&stub_fan};
    return stub_enumerate(pCount, (void **)phFan, items, 1);
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesFanGetProperties(zes_fan_handle_t hFan, zes_fan_properties_t *pProperties) {
    if (pProperties == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hFan != (zes_fan_handle_t)&stub_fan) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    memset(pProperties, 0, sizeof(*pProperties));
    pProperties->stype = ZES_STRUCTURE_TYPE_FAN_PROPERTIES;
    pProperties->pNext = NULL;
    pProperties->onSubdevice = 0;
    pProperties->subdeviceId = 0;
    pProperties->canControl = 1;
    pProperties->supportedModes = 0;
    /* RPM only. The percent path must stay unreported, which is what
     * proves `fan_unit_supported` is consulted rather than assumed. */
    pProperties->supportedUnits = 1u << ZES_FAN_SPEED_UNITS_RPM;
    pProperties->maxRPM = 4000;
    pProperties->maxPoints = 0;
    return ZE_RESULT_SUCCESS;
}

ZE_APIEXPORT ze_result_t ZE_APICALL
zesFanGetState(zes_fan_handle_t hFan, zes_fan_speed_units_t units,
               int32_t *pSpeed) {
    if (pSpeed == NULL) {
        return ZE_RESULT_ERROR_INVALID_NULL_POINTER;
    }
    if (hFan != (zes_fan_handle_t)&stub_fan) {
        return ZE_RESULT_ERROR_INVALID_ARGUMENT;
    }
    if (units != ZES_FAN_SPEED_UNITS_RPM) {
        /* Unsupported unit degrades that reading only, which is the
         * per-family independence the module header promises. */
        return ZE_RESULT_ERROR_UNSUPPORTED_FEATURE;
    }
    *pSpeed = STUB_FAN_RPM;
    return ZE_RESULT_SUCCESS;
}
