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

//! Canonical commented example emitted by `all-smi config init`. The
//! contents mirror the schema documented in issue #192 — keep this
//! text in sync with the loader whenever a new key is added.

pub const EXAMPLE_TOML: &str = r##"# all-smi configuration file
#
# All fields are optional; omitted fields fall back to the built-in
# default. Precedence from highest to lowest:
#
#   1. Explicit CLI flag  (e.g. --port 9091)
#   2. Environment variable  (e.g. ALL_SMI_API_PORT=9091)
#   3. This config file
#   4. Compiled default
#
# Environment variables follow the pattern ALL_SMI_<SECTION>_<KEY>.
# Legacy aliases from earlier releases (ALL_SMI_ALERT_TEMP,
# ALL_SMI_ENERGY_PRICE, etc.) continue to work.

# Schema version understood by this build. Do not change unless a
# future release instructs you to.
schema_version = 1

[general]
# default_mode = "local"      # "local" | "view" | "api"
# theme = "auto"              # "auto" | "light" | "dark" | "high-contrast" | "mono"
# locale = "en"

[local]
# interval_secs = 2           # 0 = adaptive default

[view]
# hostfile = "~/.config/all-smi/hosts.csv"
# hosts = []
# interval_secs = 0           # 0 = adaptive based on host count

[api]
# port = 9090
# socket = false              # bool or path (e.g. "/var/run/all-smi.sock")
# processes = false
# interval_secs = 3

[alerts]
# enabled = true
# temp_warn_c = 80
# temp_crit_c = 90
# util_idle_pct = 5
# util_idle_warn_mins = 15
# hysteresis_c = 2
# bell_on_critical = false
# webhook_url = ""            # redacted in `config print` by default

[energy]
# price_per_kwh = 0.12
# currency = "USD"
# show_cost = true
# wal_path = "~/.cache/all-smi/energy-wal.bin"
# gap_interpolate_seconds = 10
# wal_enabled = true

[display]
# color_scheme = "default"    # "default" | "colorblind" | "mono"
# gauge_style = "blocks"      # "blocks" | "braille"
# show_led_grid = true

[record]
# output_dir = "~/.cache/all-smi/records"
# compress = "zstd"           # "zstd" | "gzip" | "none"

[snapshot]
# default_format = "json"
# default_pretty = true
"##;
