use super::*;
use crate::{Permission, TransferEngine};
use serde_json::json;

#[test]
fn strict_document_rejects_compatibility_keys_unknowns_duplicates_and_wrong_types() {
    for text in [
        "[]",
        "null",
        "1",
        "{",
        r#"{"tcp":"true"}"#,
        r#"{"tcp":1}"#,
        r#"{"tcp":true,"tcp":false}"#,
        r#"{"metadata_type":"p2p"}"#,
        r#"{"metadata_servers":"https://secret.example"}"#,
        r#"{"MC_TENT_CONF":"{}"}"#,
        r#"{"transports/tcp/enable":true}"#,
        r#"{"transports":{"ub":{}}}"#,
        r#"{"hp_tcp":{"unknown":0}}"#,
        r#"{"runtime_queue":[]}"#,
        r#"{"receiver_credit":[]}"#,
        r#"{"shm":{"enable":"false"}}"#,
        r#"{"tcp":true,"transports":{"tcp":false}}"#,
        r#"{"handler_timeout_us":-1}"#,
    ] {
        let error = EngineConfig::from_json(text).unwrap_err();
        assert!(matches!(error, Error::Config { .. }), "{text}: {error}");
        assert!(!error.to_string().contains("secret.example"));
    }
    assert!(EngineConfig::from_json(&" ".repeat((1 << 20) + 1)).is_err());
    assert!(EngineConfig::from_json(r#"{"selector":{"typo":1}}"#).is_err());
    assert!(EngineConfig::from_json(r#"{"qos":{"typo":1}}"#).is_err());
}

#[test]
fn typed_fields_and_nested_budgets_reach_the_engine_snapshot() {
    let config = EngineConfig::from_json(r#"{
        "bind_addr":"127.0.0.1:0", "transports":{"tcp":true,"hp_tcp":{"enable":false},"shm":{"enable":false}},
        "merge_requests":false,"max_failover_attempts":2,"enable_auto_failover_on_poll":false,
        "max_registered_regions":8,"max_registered_bytes":8192,"max_file_segments":2,"max_peer_segments":8,
        "max_batch_requests":8,"max_allocated_batches":2,"max_allocated_requests":16,
        "max_inflight_batches":2,"max_inflight_bytes":8192,"max_transfer_bytes":4096,
        "enable_progress_worker":true,"progress_fallback_interval_us":1234,
        "notification_rpc_fallback":false,"tcp_notifications":false,
        "max_pending_notifications":8,"max_pending_notification_bytes":1024,"max_notification_bytes":128,
        "handler_threads":2,"handler_timeout_us":1000000,
        "max_receiver_credit_sessions":4,"receiver_credit":{"max_sessions":4,"max_bytes":8192,"max_requests":8,"bytes_per_sender":1024,"requests_per_sender":2},
        "runtime_queue":{"max_outstanding_owners":4,"max_outstanding_bytes":8192,"staging_owner_reserve":1,"staging_byte_reserve":1024,"max_dispatch_owners":2,"max_dispatch_bytes":4096,"deadline_aware":true,"mlu_local_threshold":0.8,"promotion_slack_ns":9000},
        "qos":{"defaults":{"priority":"low","max_inflight_bytes":4096}},
        "selector":{"policy":[{"name":"foreground","segment_type":"memory","transports":["tcp"]}]}
    }"#).unwrap();
    let engine = TransferEngine::new(config).unwrap();
    let c = engine.config();
    assert_eq!(
        (
            c.max_registered_regions,
            c.max_registered_bytes,
            c.max_file_segments,
            c.max_peer_segments
        ),
        (8, 8192, 2, 8)
    );
    assert_eq!(
        (
            c.max_batch_requests,
            c.max_allocated_batches,
            c.max_allocated_requests
        ),
        (8, 2, 16)
    );
    assert_eq!(
        (
            c.max_inflight_batches,
            c.max_inflight_bytes,
            c.max_transfer_bytes
        ),
        (2, 8192, 4096)
    );
    assert_eq!(
        (
            c.max_pending_notifications,
            c.max_pending_notification_bytes,
            c.max_notification_bytes
        ),
        (8, 1024, 128)
    );
    assert_eq!(
        (
            c.handler_threads,
            c.handler_timeout,
            c.progress_fallback_interval
        ),
        (2, Duration::from_secs(1), Duration::from_micros(1234))
    );
    assert!(
        !c.merge_requests
            && !c.enable_auto_failover_on_poll
            && !c.notification_rpc_fallback
            && !c.tcp_notifications
    );
    assert!(c.enable_progress_worker);
    assert_eq!(c.max_failover_attempts, 2);
    assert_eq!(c.receiver_credit.unwrap().max_sessions, 4);
    assert_eq!(c.runtime_queue.unwrap().staging_byte_reserve, 1024);
    assert_eq!(c.selector.as_ref().unwrap().policy[0].name, "foreground");
    let memory = vec![0; 8193].into_boxed_slice();
    assert!(matches!(
        engine.register_memory(memory, Permission::RemoteReadWrite),
        Err(Error::ResourceExhausted)
    ));
}

#[test]
fn every_engine_field_is_bound_and_wrong_typed_values_never_coerce() {
    // Every typed field must appear in the JSON dispatch. New fields cannot silently
    // become typed-only while the Python constructor depends on this loader.
    let source = include_str!("../../../src/config.rs");
    let record = source
        .split("pub struct EngineConfig {")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let loader = include_str!("../../../src/config/json/mod.rs");
    for line in record.lines().filter_map(|s| s.trim().strip_prefix("pub ")) {
        let (name, kind) = line.split_once(':').unwrap();
        let key = if kind.trim() == "Duration," {
            format!("{name}_us")
        } else {
            name.into()
        };
        assert!(
            loader.contains(&format!("config.{name} =")),
            "{key} has no binding"
        );
        let text = json!({key.clone():"wrong-type"}).to_string();
        // Some fields legitimately take strings; all numeric/bool/record fields must reject them.
        if !matches!(name, "bind_addr" | "staging_location") {
            assert!(EngineConfig::from_json(&text).is_err(), "{key}");
        }
    }
}

#[test]
fn file_inline_environment_precedence_and_immutable_child_snapshot() {
    const CHILD: &str = "ORBITE_TEST_CONFIG_CHILD";
    if let Ok(path) = std::env::var(CHILD) {
        let environment = EngineConfig::from_env().unwrap();
        assert_eq!(environment.max_file_segments, 7);
        let explicit = EngineConfig::from_json(r#"{"max_file_segments":3}"#).unwrap();
        assert_eq!(explicit.max_file_segments, 3);
        std::fs::write(&path, r#"{"max_file_segments":9}"#).unwrap();
        assert_eq!(EngineConfig::from_file(&path).unwrap().max_file_segments, 9);
        let e = TransferEngine::new(environment).unwrap();
        assert_eq!(e.config().max_file_segments, 7);
        assert_eq!(std::env::var("MC_FORCE_TCP").unwrap(), "ignored");
        return;
    }
    let dir = std::env::temp_dir().join(format!("orbite-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config.json");
    for input in [
        r#"{"max_file_segments":7}"#.to_string(),
        file.to_string_lossy().into_owned(),
    ] {
        std::fs::write(&file, r#"{"max_file_segments":7}"#).unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","config::json::tests::file_inline_environment_precedence_and_immutable_child_snapshot"])
            .env(CHILD,&file).env("ORBITE_CONF",input).env("MC_FORCE_TCP","ignored")
            .env("MC_TENT_CONF","invalid legacy text").env("TENT_METRICS_ENABLED","invalid")
            .env("MC_ENABLE_MNNVL","ignored").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(EngineConfig::from_file(&dir).is_err());
    assert!(EngineConfig::from_file(dir.join("missing")).is_err());
    std::fs::write(&file, vec![b' '; (1 << 20) + 1]).unwrap();
    assert!(EngineConfig::from_file(&file).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "rdma")]
#[test]
fn native_settings_are_typed_and_topology_file_is_captured() {
    let c = EngineConfig::from_json(r#"{"rdma":{"num_lanes":2,"signal_interval":4,"max_send_wr":64,"disable_gpu_direct_rdma":true,"log_slice_affinity":true,"rail_cooldown_us":500000,"scheduling":{"default_bandwidth_gbps":100.0}}}"#).unwrap();
    let r = c.rdma.unwrap();
    assert_eq!((r.num_lanes, r.signal_interval, r.max_send_wr), (2, 4, 64));
    assert!(r.disable_gpu_direct_rdma && r.log_slice_affinity);
    assert_eq!(r.rail_cooldown, Duration::from_millis(500));
    assert_eq!(r.scheduling.default_bandwidth_gbps, 100.0);
    for text in [r#"{"rdma":{"num_lanes":"2"}}"#, r#"{"rdma":{"bogus":1}}"#] {
        assert!(EngineConfig::from_json(text).is_err());
    }
}
