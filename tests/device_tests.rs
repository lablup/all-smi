use std::time::Duration;

use all_smi::device::common::command_executor::{
    CommandOptions, execute_command, execute_command_default,
};
use all_smi::device::common::error_handling::DeviceError;
use all_smi::device::common::json_parser::{
    json_f64, json_get, json_parse, json_string, json_u64, parse_csv_line, parse_f64, parse_u32,
    parse_u64,
};

#[test]
fn test_execute_command_default_echo() {
    let out = execute_command_default("echo", &["hello"]).expect("echo should succeed");
    assert_eq!(out.status, 0);
    assert!(out.stdout.contains("hello"));
}

#[test]
fn test_execute_command_with_status_check_error() {
    let opts = CommandOptions {
        timeout: Some(Duration::from_secs(2)),
        check_status: true,
    };
    // `false` should return a non-zero exit status on Unix-like systems
    let err = execute_command("false", &[], &opts).unwrap_err();
    match err {
        DeviceError::CommandFailed { .. } => {}
        other => panic!("Expected CommandFailed error, got: {other}"),
    }
}

#[test]
fn test_parse_csv_line_basic() {
    let parts = parse_csv_line(" 1, two , 3 ");
    assert_eq!(parts, vec!["1", "two", "3"]);
}

#[test]
fn test_number_parsers() {
    assert_eq!(parse_u64("42").unwrap(), 42);
    assert!(parse_u64("x").is_err());

    assert_eq!(parse_u32("7").unwrap(), 7);
    assert!(parse_u32("7.2").is_err());

    assert!((parse_f64("2.34").unwrap() - 2.34).abs() < 1e-9);
    assert!(parse_f64("pi").is_err());
}

#[test]
fn test_json_helpers() {
    let v = serde_json::json!({
        "a": "hello",
        "b": 123,
        "c": "456",
        "d": 1.5,
        "e": "2.5",
        "obj": { "x": 1, "y": 2 }
    });

    assert_eq!(json_string(&v, "a").unwrap(), "hello");
    assert_eq!(json_u64(&v, "b").unwrap(), 123);
    assert_eq!(json_u64(&v, "c").unwrap(), 456);
    assert!((json_f64(&v, "d").unwrap() - 1.5).abs() < 1e-9);
    assert!((json_f64(&v, "e").unwrap() - 2.5).abs() < 1e-9);
    assert!(json_get(&v, "missing").is_err());

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Obj {
        x: u32,
        y: u32,
    }
    let obj: Obj = json_parse(&v, "obj").unwrap();
    assert_eq!(obj, Obj { x: 1, y: 2 });
}
