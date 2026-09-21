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

//! Card-level power accounting on multi-die Rebellions boards (issue #418).
//!
//! `rbln-stat --json` enumerates dies, and every die of an ATOM Max card
//! repeats that card's `card_power`. These tests drive the reader's real
//! conversion step (`gpu_info_from_response`) and check that each card's
//! power is counted exactly once.
//!
//! `rbln-stat-atom-max-synthetic.json` is synthesized, not captured from
//! hardware. It follows the structure documented in issue #418 (8 cards, 32
//! dies, four dies per `sid`, `location` cycling 1..4 within a card) and the
//! note from #416 that `npu` repeats once per card on ATOM Max, in exactly
//! the JSON shape of the real ATOM Plus capture in `rbln-stat-loaded.json`.
//! Replace it with a real ATOM Max capture when one is attached to the
//! issue.
//!
//! Gated on `cli` because the assertions go through the same aggregation
//! (`metrics::gpu_readings`), `/metrics` renderer and remote parser the
//! TUI and API use, which only exist in that build.

use super::*;
use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::device::readers::detail_keys::CARD_POWER_WATTS_DETAIL_KEY;
use crate::metrics::gpu_readings::total_power_watts;
use crate::network::metrics_parser::MetricsParser;
use crate::utils::RuntimeEnvironment;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};

const ATOM_MAX: &str = include_str!("../../../tests/fixtures/rbln-stat-atom-max-synthetic.json");

/// Verbatim capture from an 8-card ATOM Plus node (one die per card).
const ATOM_PLUS_LOADED: &str = include_str!("../../../tests/fixtures/rbln-stat-loaded.json");

/// Real draw of the synthetic node: 41.2 + 42.8 + 43.1 + 42.0 + 43.5 + 41.9
/// + 42.7 + 44.3 W, one value per card.
const ATOM_MAX_CARD_TOTAL_WATTS: f64 = 341.5;

/// HBM behind one die on both boards, in bytes (15.72 GiB).
const DIE_MEMORY_BYTES: u64 = 16_877_879_296;

const TIME: &str = "2026-09-20 10:00:00";
const HOST: &str = "atom-max-01";

fn parse(json: &str) -> RblnResponse {
    serde_json::from_str(json).expect("rbln-stat output must deserialize")
}

/// One poll through a fresh reader, exactly as `get_npu_info_internal`
/// converts it.
fn poll(json: &str) -> Vec<GpuInfo> {
    RebellionsNpuReader::new().gpu_info_from_response(parse(json), TIME, HOST)
}

fn kernel_index(device: &str) -> u32 {
    device
        .strip_prefix("rbln")
        .and_then(|n| n.parse().ok())
        .expect("fixture device names are rblnN")
}

/// Per card (`sid`): the parsed card power and the uuid of the die with the
/// lowest kernel index, read straight from the fixture.
fn expected_cards(json: &str) -> BTreeMap<String, (f64, String)> {
    let mut cards: BTreeMap<String, (f64, u32, String)> = BTreeMap::new();
    for device in parse(json).devices {
        let watts = parse_power(&device.card_power).expect("fixture card_power parses");
        let index = kernel_index(&device.device);
        let entry = cards
            .entry(device.sid.clone())
            .or_insert((watts, index, device.uuid.clone()));
        if index < entry.1 {
            *entry = (watts, index, device.uuid.clone());
        }
    }
    cards
        .into_iter()
        .map(|(sid, (watts, _, uuid))| (sid, (watts, uuid)))
        .collect()
}

fn card_value(row: &GpuInfo) -> Option<&str> {
    row.detail
        .get(CARD_POWER_WATTS_DETAIL_KEY)
        .map(String::as_str)
}

fn serial_id(row: &GpuInfo) -> &str {
    row.detail
        .get("Serial ID")
        .map(String::as_str)
        .expect("every row carries its board serial")
}

/// Regression for issue #418: an 8-card ATOM Max node reported 1366 W
/// (every card counted once per die) instead of the 341.5 W it draws.
#[test]
fn atom_max_counts_each_card_power_once() {
    let rows = poll(ATOM_MAX);
    assert_eq!(rows.len(), 32, "one row per die is kept");

    // First, so a regression reports the number an operator would see.
    let total = total_power_watts(&rows);
    assert!(
        (total - ATOM_MAX_CARD_TOTAL_WATTS).abs() < 1e-6,
        "total NPU power {total} W, expected {ATOM_MAX_CARD_TOTAL_WATTS} W"
    );

    let cards = expected_cards(ATOM_MAX);
    assert_eq!(cards.len(), 8);

    let reporting: Vec<&GpuInfo> = rows
        .iter()
        .filter(|row| row.power_consumption_reading().is_some())
        .collect();
    assert_eq!(reporting.len(), 8, "exactly one reporting die per card");

    let mut reporting_sids: Vec<&str> = reporting.iter().map(|row| serial_id(row)).collect();
    reporting_sids.sort_unstable();
    reporting_sids.dedup();
    assert_eq!(reporting_sids.len(), 8, "one reporting die per sid");

    for row in &rows {
        let (watts, reporter_uuid) = &cards[serial_id(row)];
        assert_eq!(
            row.detail.get(CARD_POWER_WATTS_DETAIL_KEY),
            Some(&format!("{watts:.2}")),
            "every die of a multi-die card carries its card's power ({})",
            row.uuid
        );
        if row.uuid == *reporter_uuid {
            let power = row
                .power_consumption_reading()
                .expect("the lowest-index die reports its card");
            assert!((power - watts).abs() < 1e-9, "{power} vs {watts}");
        } else {
            assert_eq!(
                row.power_consumption_reading(),
                None,
                "only the lowest-index die of a card reports ({})",
                row.uuid
            );
        }
    }
}

/// The reporting die is chosen by kernel index, not by list position, so
/// the set of reporting rows does not move when rbln-stat lists the same
/// devices in another order.
#[test]
fn reporting_dies_are_stable_under_reordering() {
    fn reporting_uuids(rows: &[GpuInfo]) -> BTreeSet<String> {
        rows.iter()
            .filter(|row| row.power_consumption_reading().is_some())
            .map(|row| row.uuid.clone())
            .collect()
    }

    let reader = RebellionsNpuReader::new();
    let listed = reader.gpu_info_from_response(parse(ATOM_MAX), TIME, HOST);

    let mut reversed = parse(ATOM_MAX);
    reversed.devices.reverse();
    let reversed = reader.gpu_info_from_response(reversed, TIME, HOST);

    let mut by_uuid = parse(ATOM_MAX);
    by_uuid.devices.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    let by_uuid = reader.gpu_info_from_response(by_uuid, TIME, HOST);

    let expected: BTreeSet<String> = expected_cards(ATOM_MAX)
        .into_values()
        .map(|(_, uuid)| uuid)
        .collect();
    for rows in [&listed, &reversed, &by_uuid] {
        assert_eq!(reporting_uuids(rows), expected);
        assert!((total_power_watts(rows) - ATOM_MAX_CARD_TOTAL_WATTS).abs() < 1e-6);
    }
}

/// ATOM Plus is one die per card, so nothing changes there: every device
/// reports its own card, and `detail` (hence the `all_smi_gpu_info` label
/// set) gains no key.
#[test]
fn atom_plus_rows_are_unchanged() {
    let response = parse(ATOM_PLUS_LOADED);
    let sids: BTreeSet<&str> = response.devices.iter().map(|d| d.sid.as_str()).collect();
    assert_eq!(sids.len(), 8, "eight cards, one die each");

    let rows = poll(ATOM_PLUS_LOADED);
    assert_eq!(rows.len(), 8);
    let mut expected_total = 0.0;
    for (row, device) in rows.iter().zip(&response.devices) {
        let power = row
            .power_consumption_reading()
            .expect("every single-die card reports its own power");
        let own = parse_power(&device.card_power).expect("captured card_power parses");
        assert!((power - own).abs() < 1e-9, "{power} vs {own}");
        expected_total += own;

        let keys: BTreeSet<&str> = row.detail.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ATOM_PLUS_DETAIL_KEYS.iter().copied().collect(),
            "ATOM Plus detail must keep exactly its previous key set"
        );
    }
    assert!((total_power_watts(&rows) - expected_total).abs() < 1e-9);
}

/// The detail keys an ATOM Plus row carried before issue #418.
const ATOM_PLUS_DETAIL_KEYS: &[&str] = &[
    "Board Info",
    "Device Path",
    "Firmware Version",
    "KMD Version",
    "Location",
    "PCI Bus ID",
    "PCI Link Speed",
    "PCI NUMA Node",
    "PCIe Link Width",
    "Performance State",
    "Serial ID",
    "Status",
    "lib_name",
    "lib_version",
];

/// Memory is per die and must still sum per die: no row is collapsed.
#[test]
fn per_die_memory_is_unchanged() {
    for (json, dies) in [(ATOM_MAX, 32u64), (ATOM_PLUS_LOADED, 8)] {
        let rows = poll(json);
        assert_eq!(rows.len() as u64, dies);
        assert!(rows.iter().all(|row| row.total_memory == DIE_MEMORY_BYTES));
        let sum: u64 = rows.iter().map(|row| row.total_memory).sum();
        assert_eq!(sum, dies * DIE_MEMORY_BYTES);
    }
}

/// A die without a usable `sid` cannot be matched to its siblings, so it is
/// its own card and reports its own power. A missing `sid` key used to fail
/// the whole response, hiding every NPU on the host.
#[test]
fn a_die_without_a_sid_reports_its_own_power() {
    let mut json: serde_json::Value = serde_json::from_str(ATOM_MAX).expect("fixture is JSON");
    let devices = json["devices"].as_array_mut().expect("devices array");
    // Card 0 is rbln0..rbln3. Blank one die's sid, drop another's entirely.
    devices[1]["sid"] = serde_json::Value::String("  ".to_string());
    devices[2]
        .as_object_mut()
        .expect("device object")
        .remove("sid");
    let json = json.to_string();

    let rows = poll(&json);
    assert_eq!(rows.len(), 32, "the response still parses");

    let by_path = |path: &str| -> &GpuInfo {
        rows.iter()
            .find(|row| row.detail.get("Device Path").map(String::as_str) == Some(path))
            .expect("row present")
    };
    for orphan in ["rbln1", "rbln2"] {
        let row = by_path(orphan);
        let power = row
            .power_consumption_reading()
            .expect("a sid-less die reports its own power");
        assert!((power - 41.2).abs() < 1e-9, "{orphan}: {power}");
        assert_eq!(card_value(row), None);
    }
    // The rest of card 0 is still a two-die card.
    assert!(by_path("rbln0").power_consumption_reading().is_some());
    assert_eq!(by_path("rbln3").power_consumption_reading(), None);
    assert_eq!(card_value(by_path("rbln0")), Some("41.20"));
    assert_eq!(card_value(by_path("rbln3")), Some("41.20"));
    // 341.5 W, plus card 0 counted twice more through its two orphans.
    assert!((total_power_watts(&rows) - (ATOM_MAX_CARD_TOTAL_WATTS + 2.0 * 41.2)).abs() < 1e-6);
}

/// A fixture die with the fields the card grouping reads overridden.
fn die(sid: &str, device: &str, card_power: &str) -> RblnDevice {
    let mut die = parse(ATOM_MAX).devices.swap_remove(0);
    die.sid = sid.to_string();
    die.device = device.to_string();
    die.uuid = format!("uuid-{device}");
    die.card_power = card_power.to_string();
    die
}

fn reporters(roles: &[CardPowerRole]) -> Vec<usize> {
    roles
        .iter()
        .enumerate()
        .filter(|(_, role)| role.reports_card_power)
        .map(|(position, _)| position)
        .collect()
}

/// Lowest parsed kernel index wins; an unparsed name sorts after every
/// parsed one; among unparsed names rbln-stat's order decides.
#[test]
fn reporting_die_rule() {
    let devices = [
        die("A", "rbln7", "40000000uW"),
        die("A", "bogus", "40000000uW"),
        die("A", "rbln3", "40000000uW"),
        die("A", "rbln12", "40000000uW"),
        die("B", "weird", "10000000uW"),
        die("B", "odd", "10000000uW"),
    ];
    let roles = card_power_roles(&devices);
    assert_eq!(reporters(&roles), vec![2, 4]);
    let near = |watts: f64| {
        move |role: &CardPowerRole| {
            role.shared_card_watts
                .is_some_and(|shared| (shared - watts).abs() < 1e-9)
        }
    };
    assert!(roles[..4].iter().all(near(40.0)));
    assert!(roles[4..].iter().all(near(10.0)));

    // A single-die card and a sid-less die are cards of their own.
    let roles = card_power_roles(&[die("C", "rbln0", "1W"), die("", "rbln1", "1W")]);
    assert_eq!(roles, vec![CardPowerRole::SINGLE_DIE_CARD; 2]);
}

/// A reporting die whose `card_power` does not parse leaves the card with no
/// shared value rather than a made-up one.
#[test]
fn unparsable_reporter_power_publishes_no_card_value() {
    let devices = [
        die("A", "rbln0", "garbage"),
        die("A", "rbln1", "40000000uW"),
    ];
    let roles = card_power_roles(&devices);
    assert_eq!(reporters(&roles), vec![0]);
    assert!(roles.iter().all(|role| role.shared_card_watts.is_none()));
}

/// The card value is dynamic: it must refresh on every poll whether the
/// die's static details come from the cache or not.
#[test]
fn card_power_refreshes_on_cached_and_uncached_paths() {
    let reader = RebellionsNpuReader::new();
    let first = reader.gpu_info_from_response(parse(ATOM_MAX), TIME, HOST);
    assert_eq!(card_value(&first[1]), Some("41.20"));

    // Second poll, cached path, card 0 now draws 50 W.
    let busier = ATOM_MAX.replace("\"41200000uW\"", "\"50000000uW\"");
    let second = reader.gpu_info_from_response(parse(&busier), TIME, HOST);
    assert!(reader.get_device_static_info(&second[1].uuid).is_some());
    assert_eq!(card_value(&second[1]), Some("50.00"));

    // Uncached path: a reader whose cache was built from another host.
    let other = RebellionsNpuReader::new();
    other.gpu_info_from_response(parse(ATOM_PLUS_LOADED), TIME, HOST);
    let uncached = other.gpu_info_from_response(parse(&busier), TIME, HOST);
    assert!(other.get_device_static_info(&uncached[1].uuid).is_none());
    assert_eq!(card_value(&uncached[1]), Some("50.00"));
    assert!((total_power_watts(&uncached) - (ATOM_MAX_CARD_TOTAL_WATTS + 8.8)).abs() < 1e-6);
}

fn render(rows: &[GpuInfo]) -> String {
    let env = RuntimeEnvironment::default();
    render_prometheus_exposition(&MetricsRenderInputs {
        gpu_info: rows,
        process_info: &[],
        cpu_info: &[],
        memory_info: &[],
        storage_info: &[],
        runtime_environment: &env,
        chassis_info: &[],
        vgpu_info: &[],
        mig_info: &[],
        energy_integrator: None,
        ready: true,
    })
}

fn sample_value(line: &str) -> f64 {
    line.rsplit(' ')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(f64::NAN)
}

fn lines_of<'a>(exposition: &'a str, family: &str) -> Vec<&'a str> {
    let prefix = format!("{family}{{");
    exposition
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

/// The same node one poll later, with every card drawing 1 W more.
fn with_card_power_bumped(json: &str) -> String {
    let re = Regex::new(r#""card_power": "(\d+)uW""#).expect("regex");
    re.replace_all(json, |caps: &regex::Captures| {
        let micro_watts: u64 = caps[1].parse().expect("fixture card_power is numeric");
        format!(r#""card_power": "{}uW""#, micro_watts + 1_000_000)
    })
    .into_owned()
}

/// Strip the SGR sequences `print_gpu_info` writes, so an assertion can look
/// at the text an operator reads. Mirrors the private `render_row` helper in
/// `ui::renderers::gpu_renderer`.
fn render_row(info: &GpuInfo) -> String {
    let mut buf: Vec<u8> = Vec::new();
    crate::ui::renderers::gpu_renderer::print_gpu_info(&mut buf, 0, info, 120, 0, 0, false);
    let raw = String::from_utf8_lossy(&buf).into_owned();
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Issue #425: `all_smi_gpu_info` identifies the device, so its label set
/// must be identical across two polls whose card powers differ. Before the
/// fix, each of the 32 dies started a new Prometheus series per scrape.
#[test]
fn atom_max_identity_labels_hold_still_while_card_power_moves() {
    let first = render(&poll(ATOM_MAX));
    let second = render(&poll(&with_card_power_bumped(ATOM_MAX)));

    let identity = |exposition: &str| -> Vec<String> {
        lines_of(exposition, "all_smi_gpu_info")
            .into_iter()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(identity(&first).len(), 32);
    assert_eq!(
        identity(&first),
        identity(&second),
        "a card power change moved the identity label set"
    );

    // The reading itself did move; it just moved on its own family.
    let card = |exposition: &str| -> Vec<String> {
        lines_of(exposition, "all_smi_gpu_card_power_watts")
            .into_iter()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(card(&first).len(), 32);
    assert_ne!(
        card(&first),
        card(&second),
        "the board readings must still reach Prometheus"
    );
}

/// `/metrics` for the ATOM Max node: one power series per card, never a
/// sentinel, and the card value on its own gauge rather than as a label.
#[test]
fn atom_max_exposition_counts_each_card_once() {
    let exposition = render(&poll(ATOM_MAX));
    let samples: Vec<&str> = exposition
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();

    let power: Vec<f64> = samples
        .iter()
        .filter(|line| line.starts_with("all_smi_gpu_power_consumption_watts{"))
        .map(|line| sample_value(line))
        .collect();
    assert_eq!(power.len(), 8, "one power series per card");
    let sum: f64 = power.iter().sum();
    assert!((sum - ATOM_MAX_CARD_TOTAL_WATTS).abs() < 1e-6, "sum {sum}");

    for line in samples.iter().filter(|line| {
        line.split('{')
            .next()
            .is_some_and(|name| name.contains("power"))
    }) {
        let value = sample_value(line);
        assert!(value.is_finite() && value >= 0.0, "sentinel leaked: {line}");
    }
    assert!(
        !samples
            .iter()
            .any(|line| line.starts_with("all_smi_npu_power_watts{")),
        "the card key must not feed the generic NPU power series"
    );

    let info: Vec<&&str> = samples
        .iter()
        .filter(|line| line.starts_with("all_smi_gpu_info{"))
        .collect();
    assert_eq!(info.len(), 32);
    // Issue #425 inverted this: the board power is a live reading, so it is
    // no longer a label on the identity series.
    assert!(
        !info
            .iter()
            .any(|line| line.contains(&format!("{CARD_POWER_WATTS_DETAIL_KEY}=\""))),
        "the card value must not be an identity label"
    );

    // It is carried by its own family instead: every die repeats its board's
    // value, which is exactly why summing this one is wrong.
    let card = lines_of(&exposition, "all_smi_gpu_card_power_watts");
    assert_eq!(card.len(), 32, "every die carries its board's value");
    let distinct: BTreeSet<String> = card
        .iter()
        .map(|line| format!("{:.2}", sample_value(line)))
        .collect();
    assert_eq!(distinct.len(), 8, "one value per card: {distinct:?}");
    let card_sum: f64 = card.iter().map(|line| sample_value(line)).sum();
    assert!(
        (card_sum - 4.0 * ATOM_MAX_CARD_TOTAL_WATTS).abs() < 1e-6,
        "summing the card family overcounts by the die count, as documented: {card_sum}"
    );
}

/// A remote viewer scraping that exposition reconstructs the same total and
/// keeps the card value for display.
#[test]
fn atom_max_exposition_round_trips_through_the_remote_parser() {
    let exposition = render(&poll(ATOM_MAX));
    let re = Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").expect("regex");
    let parsed = MetricsParser::new().parse_metrics(&exposition, "atom-max-01:9090", &re);

    assert_eq!(parsed.gpu_info.len(), 32);
    let total = total_power_watts(&parsed.gpu_info);
    assert!(
        (total - ATOM_MAX_CARD_TOTAL_WATTS).abs() < 1e-6,
        "total {total}"
    );
    assert_eq!(
        parsed
            .gpu_info
            .iter()
            .filter(|row| row.power_consumption_reading().is_none())
            .count(),
        24
    );

    let cards = expected_cards(ATOM_MAX);
    let sid_by_uuid: BTreeMap<String, String> = parse(ATOM_MAX)
        .devices
        .into_iter()
        .map(|device| (device.uuid, device.sid))
        .collect();
    for row in &parsed.gpu_info {
        let (watts, _) = &cards[&sid_by_uuid[&row.uuid]];
        assert_eq!(
            row.detail.get(CARD_POWER_WATTS_DETAIL_KEY),
            Some(&format!("{watts:.2}")),
            "card value lost in transit for {}",
            row.uuid
        );
    }

    // What the operator actually sees. A die with no power series of its own
    // still shows its board's draw in parentheses in remote view, matching
    // `die_without_own_power_shows_its_card_value_in_parentheses` in local
    // view. This is the behavior the #425 re-routing had to preserve: the
    // value now arrives as `all_smi_gpu_card_power_watts` rather than as an
    // `all_smi_gpu_info` label.
    let silent = parsed
        .gpu_info
        .iter()
        .find(|row| row.power_consumption_reading().is_none())
        .expect("three dies per card report no power of their own");
    let (watts, _) = &cards[&sid_by_uuid[&silent.uuid]];
    let rendered = render_row(silent);
    assert!(
        rendered.contains(&format!("({watts:.0}W)")),
        "remote view lost the board value for {}: {rendered}",
        silent.uuid
    );
    assert!(!rendered.contains("Pwr:     N/A"), "{rendered}");
}
