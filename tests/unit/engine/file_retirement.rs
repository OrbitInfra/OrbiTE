use super::*;
use crate::transport::gds::tests::Mock;
use std::sync::atomic::Ordering;

#[test]
fn gds_non_file_group_keeps_common_retirement_and_metrics() -> Result<()> {
    let mock = Mock::new(crate::GdsMode::NativeOnly);
    let config = EngineConfig {
        runtime_queue: Some(Default::default()),
        ..Default::default()
    };
    let mut engine = TransferEngine::new(config.clone())?;
    mock.install(&mut engine, &config);
    let source = TransferEngine::new(Default::default())?;
    let remote = source.register_memory(vec![0x31; 32].into(), Permission::RemoteReadWrite)?;
    let local = engine.register_memory(vec![0x72; 32].into(), Permission::LocalOnly)?;
    let wait = crate::test_support::EVENTUAL_PROGRESS;
    let peer = engine.open_segment(source.endpoint(), source.source_epoch(), wait)?;
    let requests: Vec<_> = [1, 17]
        .map(|at| {
            TransferRequest::new(
                Transport::Tcp,
                local.base_address() + at,
                remote.base_address() + at,
                8,
            )
        })
        .into();
    let prepared = engine.prepare(TransferOp::Write, &peer, &requests, None, wait, None)?;
    assert!(matches!(prepared.payload, PreparedTarget::Memory { .. }));
    assert_eq!(prepared.owners.len(), 2);
    #[cfg(feature = "metrics")]
    prepared.accept_metrics();
    assert_eq!(engine.stats()?.runtime_queue.unwrap().queued_owners, 2);

    for first in 0..2 {
        // Force the defensive non-File arm. It has no cuFile buffer leases,
        // but an unposted rejection still owes admission and metric receipts.
        let error = Error::InvalidConfig("GDS requires a file target");
        #[cfg(feature = "metrics")]
        if let Some(m) = &prepared.owners[first].metrics {
            m.begin(Transport::Gds);
            m.finish(Some(&error), false);
        }
        let report = engine.finish_file_group(
            &prepared,
            first,
            1,
            None,
            BatchReport {
                completed_requests: 0,
                transferred_bytes: 0,
                failure: Some(TransferFailure {
                    request_index: 0,
                    affected_requests: 1,
                    error,
                    remote_effect: RemoteEffect::None,
                }),
                locally_drained: true,
                notification: NotificationStatus::NotRequested,
            },
        );
        assert!(report.locally_drained);
        assert_eq!(report.completed_requests, 0);
        assert!(matches!(
            report.failure.unwrap().error,
            Error::InvalidConfig(_)
        ));
        // The final permit deliberately follows PreparedSubmission teardown.
        let queue = engine.stats()?.runtime_queue.unwrap();
        assert_eq!((queue.queued_owners, queue.queued_bytes), (1, 8));
        #[cfg(feature = "metrics")]
        {
            let text = engine.metrics().unwrap().prometheus();
            for expected in [
                format!(
                    "orbite_logical_request_outcomes_total{{operation=\"write\",outcome=\"failed\"}} {}",
                    first + 1
                ),
                format!(
                    "orbite_logical_requests_inflight{{operation=\"write\"}} {}",
                    1 - first
                ),
                format!(
                    "orbite_transport_attempt_outcomes_total{{transport=\"gds\",operation=\"write\",outcome=\"failed\"}} {}",
                    first + 1
                ),
                "orbite_inflight_attempts{transport=\"gds\"} 0".into(),
            ] {
                assert!(
                    text.lines().any(|line| line == expected),
                    "missing {expected}"
                );
            }
        }
    }
    drop(prepared);
    let stats = engine.stats()?;
    let queue = stats.runtime_queue.unwrap();
    assert_eq!((queue.queued_owners, queue.queued_bytes), (0, 0));
    assert_eq!((queue.dispatching_owners, queue.dispatching_bytes), (0, 0));
    assert_eq!((stats.inflight_batches, stats.inflight_bytes), (0, 0));
    assert_eq!(stats.active_peer_operations, 0);
    for count in [
        &mock.submits,
        &mock.buffer_registers,
        &mock.buffer_deregisters,
        &mock.file_registers,
        &mock.destroys,
    ] {
        assert_eq!(count.load(Ordering::Acquire), 0);
    }
    let mut bytes = [0; 32];
    local.read(0, &mut bytes)?;
    assert_eq!(bytes, [0x72; 32]);
    remote.read(0, &mut bytes)?;
    assert_eq!(bytes, [0x31; 32]);
    engine.close(wait)?;
    source.close(wait)?;
    Ok(())
}
