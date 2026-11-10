use super::*;
#[test]
fn pairs_follow_the_start_and_only_emit_bounded_outcomes() {
    let cells = Arc::new(Recorder::new());
    let scrape = Scrape {
        recorder: cells.clone(),
        owners: Box::new(Owners::default),
    };
    for (reason, outcome) in [
        (None, "success"),
        (Some(Error::Unavailable("unposted")), "failed"),
        (Some(Error::Timeout), "timeout"),
        (Some(Error::Cancelled), "canceled"),
    ] {
        let make = || Observation::new(&cells, TransferOp::Write, Transport::Rdma, 1, None, None);
        let recorded = make();
        cells.enabled.store(true, Relaxed);
        recorded.accept_public();
        recorded.accept_public();
        recorded.begin(Transport::Rdma);
        cells.enabled.store(false, Relaxed);
        let ignored = make();
        ignored.accept_public();
        ignored.begin(Transport::Rdma);
        recorded.finish(reason.as_ref(), false);
        recorded.terminal(reason.as_ref());
        cells.enabled.store(true, Relaxed);
        ignored.finish(reason.as_ref(), false);
        ignored.terminal(reason.as_ref());
        recorded.terminal(None);
        let text = scrape.prometheus();
        assert!(text.contains(&format!(
            r#"orbite_logical_request_outcomes_total{{operation="write",outcome="{outcome}"}} 2"#
        )));
        assert!(text.contains(&format!(r#"orbite_transport_attempt_outcomes_total{{transport="rdma",operation="write",outcome="{outcome}"}} 1"#)));
    }
    let j = scrape.json();
    assert_eq!(j["orbite_logical_requests_started_total"], 8);
    assert_eq!(j["orbite_transport_attempts_total"], 4);
    assert_eq!(j["orbite_logical_requests_inflight"], 0);
    assert_eq!(j["orbite_inflight_attempts"], 0);
}

#[test]
fn exporter_health_survives_pause_and_failed_serialization() {
    if crate::test_ports::isolated().is_some() {
        return;
    }
    let e = engine();
    let m = e.metrics().unwrap();
    m.set_enabled(false);
    let health = e.metrics_status();
    assert_eq!(health["orbite_metrics_http_server_up"], 1);
    assert_eq!(health["orbite_metrics_runtime_enabled"], 0);
    assert_eq!(
        health["orbite_metrics_build_info"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        health["orbite_metrics_build_info"]["commit"],
        crate::diagnostics::BUILD_COMMIT
    );
    for (format, path) in [("prometheus", "/metrics"), ("json", "/metrics/json")] {
        m.recorder().pipeline.fail_next.store(true, Relaxed);
        let (head, _) = get(
            m.http_port(),
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        );
        assert!(head.starts_with("HTTP/1.1 500"));
        let h = e.metrics_status();
        for key in [
            "orbite_metrics_serialization_attempts_total",
            "orbite_metrics_serialization_failures_total",
        ] {
            assert_eq!(h[key][format], 1);
        }
        assert_eq!(
            h["orbite_metrics_last_success_timestamp_seconds"][format],
            0
        );
        let body = scrape(m.http_port(), path);
        if format == "json" {
            let j: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(j["orbite_metrics_serialization_attempts_total"]["json"], 2);
            assert!(
                j["orbite_metrics_last_success_timestamp_seconds"]["json"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
        } else {
            assert!(
                body.contains(
                    r#"orbite_metrics_serialization_failures_total{format="prometheus"} 1"#
                )
            );
            assert!(body.contains("orbite_metrics_runtime_enabled 0"));
        }
        assert_eq!(
            e.metrics_status()["orbite_metrics_serialization_attempts_total"][format],
            2
        );
        assert!(
            e.metrics_status()["orbite_metrics_last_success_timestamp_seconds"][format]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(scrape(m.http_port(), "/health"), "OK");
    }
    let off = TransferEngine::new(EngineConfig {
        metrics: MetricsConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    assert!(off.metrics().is_none());
    let h = off.metrics_status();
    for key in [
        "orbite_metrics_runtime_enabled",
        "orbite_metrics_http_server_up",
        "orbite_metrics_http_start_failures_total",
    ] {
        assert_eq!(h[key], 0);
    }
    assert_eq!(
        h["orbite_metrics_build_info"],
        health["orbite_metrics_build_info"]
    );
}

#[test]
fn rejected_preflight_records_no_start_and_a_failed_prefix_settles_its_suffix() {
    if crate::test_ports::isolated().is_some() {
        return;
    }
    let source = engine();
    let requester = engine();
    let remote = source
        .register_memory(vec![0x5a; 12].into(), Permission::RemoteReadWrite)
        .unwrap();
    let local = requester
        .register_memory(vec![0xee; 12].into(), Permission::LocalOnly)
        .unwrap();
    let peer = requester
        .open_segment(source.endpoint(), source.source_epoch(), WAIT)
        .unwrap();
    let req = |hint, off| {
        TransferRequest::new(
            hint,
            local.base_address() + off,
            remote.base_address() + off,
            2,
        )
    };
    let m = requester.metrics().unwrap();
    assert!(
        requester
            .submit_and_wait(
                TransferOp::Write,
                &peer,
                &[req(Transport::Tcp, 1), req(Transport::Gds, 5)],
                WAIT
            )
            .is_err()
    );
    assert_eq!(m.json()["orbite_logical_requests_started_total"], 0);
    source.close_source(WAIT).unwrap();
    let r = requester
        .submit_and_wait(
            TransferOp::Write,
            &peer,
            &[req(Transport::Tcp, 1), req(Transport::Tcp, 5)],
            WAIT,
        )
        .unwrap();
    assert_eq!(r.completed_requests, 0);
    assert!(r.failure.is_some());
    let j = m.json();
    assert_eq!(j["orbite_logical_requests_started_total"], 2);
    assert_eq!(j["orbite_logical_request_outcomes_total"], 2);
    assert_eq!(j["orbite_transport_attempts_total"], 1);
    assert_eq!(j["orbite_transport_attempt_outcomes_total"], 1);
    assert_eq!(j["orbite_logical_requests_inflight"], 0);
    let mut got = [0; 12];
    local.read(0, &mut got).unwrap();
    assert_eq!(got, [0xee; 12]);
    remote.read(0, &mut got).unwrap();
    assert_eq!(got, [0x5a; 12]);
}

#[test]
fn prometheus_metric_family_contract() {
    let cells = Arc::new(Recorder::new());
    // Populate one series in each family. Check the rendered types against the frozen registry.
    for group in &cells.cells {
        let s = &group[0];
        s.seen.store(true, Relaxed);
        if let Some(bucket) = s.buckets.first() {
            bucket.store(1, Relaxed);
        }
    }
    let scrape = Scrape {
        recorder: cells,
        owners: Box::new(Owners::default),
    };
    let text = scrape.prometheus();
    for (name, kind) in [
        ("orbite_read_bytes_total", "counter"),
        ("orbite_write_bytes_total", "counter"),
        ("orbite_read_requests_total", "counter"),
        ("orbite_write_requests_total", "counter"),
        ("orbite_read_failures_total", "counter"),
        ("orbite_write_failures_total", "counter"),
        ("orbite_transport_failover_total", "counter"),
        ("orbite_transport_attempts_total", "counter"),
        ("orbite_transport_attempt_failures_total", "counter"),
        ("orbite_logical_requests_started_total", "counter"),
        ("orbite_logical_request_outcomes_total", "counter"),
        ("orbite_transport_attempt_outcomes_total", "counter"),
        ("orbite_task_failures_total", "counter"),
        ("orbite_deadline_infeasible_total", "counter"),
        ("orbite_quarantined_batches_total", "counter"),
        ("orbite_inflight_attempts", "gauge"),
        ("orbite_logical_requests_inflight", "gauge"),
        ("orbite_registered_buffer_bytes", "gauge"),
        ("orbite_read_latency_us", "histogram"),
        ("orbite_write_latency_us", "histogram"),
        ("orbite_read_size_bytes", "histogram"),
        ("orbite_write_size_bytes", "histogram"),
        ("orbite_deadline_mlu_permille", "histogram"),
        ("orbite_stage_queue_wait_us", "histogram"),
        ("orbite_stage_dispatch_us", "histogram"),
        ("orbite_stage_transport_us", "histogram"),
        ("orbite_transport_attempt_latency_us", "histogram"),
        ("orbite_metrics_build_info", "gauge"),
        ("orbite_metrics_runtime_enabled", "gauge"),
        ("orbite_metrics_http_server_up", "gauge"),
        ("orbite_metrics_http_start_failures_total", "counter"),
        ("orbite_metrics_serialization_attempts_total", "counter"),
        ("orbite_metrics_serialization_failures_total", "counter"),
        ("orbite_metrics_last_success_timestamp_seconds", "gauge"),
    ] {
        let expected = format!("# TYPE {name} {kind}");
        assert!(text.lines().any(|line| line == expected), "{name}");
    }
    assert_eq!(
        text.lines()
            .filter(|s| s.starts_with("# TYPE orbite_"))
            .count(),
        34
    );
    assert!(!text.contains("compile_time_enabled"));
}
