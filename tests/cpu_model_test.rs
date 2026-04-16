#![cfg(feature = "cli")]

use all_smi::device::CpuPlatformType;
use all_smi::network::metrics_parser::MetricsParser;
use regex::Regex;

#[test]
fn test_cpu_model_metric_parsing() {
    let parser = MetricsParser::new();
    let re = Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap();
    let host = "127.0.0.1:10001";

    let test_data = r#"
# HELP all_smi_cpu_model CPU model name
# TYPE all_smi_cpu_model info
all_smi_cpu_model{instance="node-0001", model="AMD EPYC 7763"} 1
all_smi_cpu_utilization{cpu_model="", instance="node-0001", index="0"} 50.0
all_smi_cpu_core_count{cpu_model="", instance="node-0001", index="0"} 128
all_smi_cpu_frequency_mhz{instance="node-0001"} 2450
"#;

    let result = parser.parse_metrics(test_data, host, &re);
    let cpu_info = &result.cpu_info;

    assert_eq!(cpu_info.len(), 1);
    let cpu = &cpu_info[0];
    assert_eq!(cpu.cpu_model, "AMD EPYC 7763");
    assert_eq!(cpu.utilization, 50.0);
    assert_eq!(cpu.total_cores, 128);
    assert_eq!(cpu.base_frequency_mhz, 2450);
    assert!(matches!(cpu.platform_type, CpuPlatformType::Amd));
}
