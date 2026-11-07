use super::*;
fn load(value: Value) -> crate::Result<MetricsConfig> {
    Ok(EngineConfig::from_json(&json!({"metrics":value}).to_string())?.metrics)
}
#[test]
fn metrics_defaults_and_all_typed_fields_are_reachable() {
    assert_eq!(load(json!({})).unwrap(), MetricsConfig::default());
    let configured = load(json!({"enabled":false,"http_port":6000,"http_host":"::1","http_server_threads":8,"report_interval_seconds":0})).unwrap();
    assert_eq!(
        configured,
        MetricsConfig {
            enabled: false,
            http_port: 6000,
            http_host: "::1".into(),
            http_server_threads: 8,
            report_interval_seconds: 0
        }
    );
}
#[test]
fn invalid_settings_are_typed_and_bucket_keys_are_rejected() {
    for doc in [
        json!({"http_port":0}),
        json!({"http_port":65536}),
        json!({"enabled":"true"}),
        json!({"http_server_threads":0}),
        json!({"http_server_threads":65}),
        json!({"report_interval_seconds":-1}),
        json!({"http_port":"8080"}),
        json!({"latency_buckets":[1,2]}),
        json!({"mlu_buckets":[]}),
        json!([]),
    ] {
        assert!(load(doc).is_err());
    }
    for host in [
        "",
        "host/path",
        ":",
        "bad::host",
        "-invalid",
        "two..labels",
        "999.999.0.1",
        &"a".repeat(64),
    ] {
        assert!(load(json!({"http_host":host})).is_err());
    }
    for host in ["0.0.0.0", "::1", "localhost", "rank-1.example."] {
        assert_eq!(load(json!({"http_host":host})).unwrap().http_host, host);
    }
    for port in [1, 8080, 65535] {
        assert_eq!(load(json!({"http_port":port})).unwrap().http_port, port);
    }
}
