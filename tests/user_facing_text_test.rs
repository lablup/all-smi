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

use all_smi::cli::build_command_with_runtime_help;
use clap::Command;
use regex::Regex;

const IMPLEMENTATION_PHRASES: &[(&str, &str)] = &[
    ("Rust optional type", "option<bool>"),
    ("Rust module path", "crate::"),
    ("source-tree path", "src/"),
    ("Rust keyword rationale", "rust keyword"),
    ("review-scope wording", "out of scope"),
    ("implementation specification", "issue spec"),
    ("tracker-status wording", "tracked separately"),
    ("internal cache helper", "dirs::cache_dir"),
];

fn tracker_reference() -> Regex {
    Regex::new(
        r"(?i)\b(?:issue|pr|pull request)\s*#?\s*\d+\b|(?:^|[^[:alnum:]_/])#\d+\b|\bissue spec(?:ification)?\b",
    )
    .expect("tracker-reference regex must compile")
}

fn collect_help(command: &Command, path: &str, surfaces: &mut Vec<(String, String)>) {
    let mut short = command.clone();
    surfaces.push((format!("{path} -h"), short.render_help().to_string()));

    let mut long = command.clone();
    surfaces.push((
        format!("{path} --help"),
        long.render_long_help().to_string(),
    ));

    for subcommand in command.get_subcommands() {
        collect_help(
            subcommand,
            &format!("{path} {}", subcommand.get_name()),
            surfaces,
        );
    }
}

fn assert_surface_is_operator_facing(surface: &str, text: &str) {
    let tracker = tracker_reference();
    assert!(
        tracker.find(text).is_none(),
        "{surface} contains an internal tracker reference {:?}:\n{text}",
        tracker.find(text).map(|found| found.as_str())
    );

    let lowercase = text.to_ascii_lowercase();
    for (kind, phrase) in IMPLEMENTATION_PHRASES {
        assert!(
            !lowercase.contains(phrase),
            "{surface} contains {kind} `{phrase}`:\n{text}"
        );
    }
}

fn installed_roff_text(manpage: &str) -> String {
    manpage
        .lines()
        .filter(|line| !line.trim_start().starts_with(".\\\""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_directly_addressable_help_page_is_operator_facing() {
    let command = build_command_with_runtime_help();
    let mut surfaces = Vec::new();
    collect_help(&command, command.get_name(), &mut surfaces);

    assert!(
        surfaces
            .iter()
            .any(|(path, _)| path == "all-smi service run -h"),
        "recursive help traversal must include the hidden `service run` command"
    );

    for (surface, text) in surfaces {
        assert_surface_is_operator_facing(&surface, &text);
    }
}

#[test]
fn readme_operator_text_is_free_of_internal_churn() {
    assert_surface_is_operator_facing("README.md", include_str!("../README.md"));
}

#[test]
fn installed_manpage_is_free_of_internal_churn() {
    let source = include_str!("../docs/man/all-smi.1");
    let installed_text = installed_roff_text(source);

    assert_surface_is_operator_facing("docs/man/all-smi.1", &installed_text);
    assert!(
        installed_text.contains("https://github.com/lablup/all-smi/issues"),
        "docs/man/all-smi.1 must retain the official bug-report URL"
    );
}

#[test]
fn tracker_guard_ignores_normal_operator_prose_and_roff_comments() {
    let ordinary = "## Configuration\nReport bugs at https://github.com/lablup/all-smi/issues.\nDependency issues are reported at startup.";
    assert!(
        tracker_reference().find(ordinary).is_none(),
        "tracker guard must not match Markdown headings, the official issues URL, or ordinary prose"
    );

    let roff = ".\\\" Maintainer note for issue #123\n.SH BUGS\nReport bugs at: https://github.com/lablup/all-smi/issues";
    let installed_text = installed_roff_text(roff);
    assert!(
        tracker_reference().find(&installed_text).is_none(),
        "tracker guard must ignore roff comments that are absent from the installed manpage"
    );
}
