use super::*;

#[test]
fn a_public_batch_posts_independent_lanes_and_reports_each_ack() {
    for fail in [false, true] {
        let mut c = cfg();
        c.hp_tcp.connections_per_peer = 2;
        let (_src, dst, remote, local, mut peer) = setup(c, 18);
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sockets::redirect(&mut peer, &[&l]);
        let base = remote.base_address();
        let h = thread::spawn(move || {
            let mut accepted = Vec::new();
            for _ in 0..2 {
                let (mut s, _) = accept(&l);
                s.set_read_timeout(Some(WAIT)).unwrap();
                let req = request(&mut s);
                let mut b = [0; 4];
                s.read_exact(&mut b).unwrap();
                assert_eq!(b, [0xee; 4]);
                accepted.push((s, req));
            }
            let mut target = [0xa5; 18];
            for (mut s, req) in accepted {
                let offset = (req.address - base) as usize;
                target[offset..offset + 4].fill(0xee);
                if !(fail && offset == 1) {
                    s.write_all(
                        &(Response {
                            id: req.id,
                            status: Status::Ok,
                            committed: 4,
                        })
                        .encode(),
                    )
                    .unwrap();
                }
            }
            assert_eq!(
                target,
                [
                    0xa5, 0xee, 0xee, 0xee, 0xee, 0xa5, 0xa5, 0xa5, 0xa5, 0xee, 0xee, 0xee, 0xee,
                    0xa5, 0xa5, 0xa5, 0xa5, 0xa5
                ]
            );
        });
        let requests = [1, 9].map(|off| {
            TransferRequest::new(Transport::Unspec, local.base_address() + off, base + off, 4)
        });
        let batch = dst
            .submit_async(TransferOp::Write, &peer, &requests, None, WAIT)
            .unwrap();
        batch.wait(WAIT).unwrap();
        h.join().unwrap();
        let status = batch.task_statuses().unwrap();
        assert_eq!(status[1].state, crate::TaskState::Completed);
        assert_eq!(status[0].transport, Transport::HpTcp);
        assert_eq!(status[0].state == crate::TaskState::Failed, fail);
        if fail {
            assert_eq!(
                status[0].failure.as_ref().unwrap().remote_effect,
                RemoteEffect::Unknown
            );
        }
        assert_eq!(bytes(&local), [0xee; 18]);
        assert_eq!(bytes(&remote), [0xa5; 18]);
    }
}

#[test]
fn a_rebound_port_cannot_invalidate_the_old_hp_tcp_shutdown_receipt() {
    if crate::test_ports::isolated().is_some() {
        return;
    }
    let (listener, _guard) = crate::test_ports::guarded_port();
    let endpoint = listener.local_addr().unwrap();
    let mut config = cfg();
    config.hp_tcp.bind_address = "127.0.0.1".into();
    config.hp_tcp.port = endpoint.port();
    drop(listener);
    let engine = TransferEngine::new(config).unwrap();
    let hp = engine.hp_transport_for_test();
    engine.close(WAIT).unwrap();
    assert!(matches!(hp.reserve(1), Err(Error::RequesterClosed)));
    // This independent listener had no sessions: immediate rebinding must not
    // depend on the busy-lane control's previous TCP connections or close state.
    // Force another listener to own the address after the owner's close receipt.
    let replacement = TcpListener::bind(endpoint).unwrap();
    let probe = TcpStream::connect_timeout(&endpoint, WAIT).unwrap();
    let (accepted, client) = accept(&replacement);
    assert_eq!(client, probe.local_addr().unwrap());
    assert_eq!(accepted.local_addr().unwrap(), endpoint);
    // The old connect_timeout(...).is_err() assertion fails deterministically.
    assert!(matches!(hp.reserve(1), Err(Error::RequesterClosed)));
}

#[test]
fn dropping_a_busy_lane_fences_new_work_and_physical_close_joins_sessions() {
    if crate::test_ports::isolated().is_some() {
        return;
    }
    let dst = TransferEngine::new(cfg()).unwrap();
    // Keep the explicit port out of other fixtures' bind(0) allocation range.
    let (listener, _guard) = crate::test_ports::guarded_port();
    let endpoint = listener.local_addr().unwrap();
    let mut config = cfg();
    config.hp_tcp.bind_address = "127.0.0.1".into();
    config.hp_tcp.port = endpoint.port();
    drop(listener);
    let src = TransferEngine::new(config).unwrap();
    let remote = src
        .register_memory(vec![0xa5; 12].into(), Permission::RemoteReadWrite)
        .unwrap();
    let local = dst
        .register_memory(vec![0xee; 12].into(), Permission::LocalOnly)
        .unwrap();
    let peer = dst
        .open_segment(src.endpoint(), src.source_epoch(), WAIT)
        .unwrap();
    assert_eq!(endpoint, address(&peer).into());
    let request = TransferRequest::new(
        Transport::HpTcp,
        local.base_address() + 1,
        remote.base_address() + 1,
        8,
    );
    assert!(
        dst.submit_and_wait(TransferOp::Read, &peer, &[request], WAIT)
            .unwrap()
            .is_complete()
    );
    assert_eq!(
        bytes(&local),
        [
            0xee, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xee, 0xee, 0xee
        ]
    );
    let hp = src.hp_transport_for_test();
    let handles = hp.workers.handles();
    let (entered, started) = mpsc::channel();
    let (retired, dropped) = mpsc::channel();
    struct OnDrop(mpsc::Sender<usize>, usize);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(self.1);
        }
    }
    let jobs: Vec<_> = handles
        .iter()
        .enumerate()
        .map(|(index, handle)| {
            let (entered, retired) = (entered.clone(), retired.clone());
            handle.spawn(async move {
                let _receipt = OnDrop(retired, index);
                entered.send(()).unwrap();
                std::future::pending::<()>().await;
            })
        })
        .collect();
    for _ in &handles {
        started.recv_timeout(WAIT).unwrap();
    }
    // A complete guarded READ proves each socket was accepted, not just queued
    // by the kernel. Leave its next frame incomplete while physical close runs.
    let mut sessions: Vec<_> = handles
        .iter()
        .map(|_| {
            let mut socket = connect(address(&peer));
            socket
                .write_all(&frame(&peer, TransferOp::Read, 1, 8).encode())
                .unwrap();
            let reply = response(&mut socket);
            assert_eq!((reply.status, reply.committed), (Status::Ok, 8));
            let mut data = [0xcc; 10];
            socket.read_exact(&mut data[1..9]).unwrap();
            assert_eq!(
                data,
                [0xcc, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xcc]
            );
            socket.write_all(&[0]).unwrap();
            socket
        })
        .collect();
    src.close(WAIT).unwrap();
    src.close(WAIT).unwrap();
    // close joins every reactor. Already-delivered task-drop receipts and
    // finished tasks prove those owners retired without observing a port.
    let mut receipts: Vec<_> = (0..handles.len())
        .map(|_| dropped.try_recv().unwrap())
        .collect();
    receipts.sort_unstable();
    assert_eq!(receipts, (0..handles.len()).collect::<Vec<_>>());
    assert!(jobs.iter().all(tokio::task::JoinHandle::is_finished));
    assert!(matches!(hp.reserve(1), Err(Error::RequesterClosed)));
    for session in &mut sessions {
        let error = session.read_exact(&mut [0]).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ),
            "{error}"
        );
    }
    assert_eq!(bytes(&remote), [0xa5; 12]);
}

#[cfg(feature = "rdma")]
#[test]
fn hp_tcp_quota_and_unposted_connect_failures_select_the_next_rdma_candidate() {
    use crate::engine::tests::rdma_fixture_with_config;
    use crate::transport::rdma::tests::runtime;
    let mut c = cfg();
    c.hp_tcp.max_outstanding_bytes = 4;
    c.selector=Some(crate::TransportSelectorConfig::from_json(r#"{"policy":[{"name":"host","segment_type":"memory","transports":["hp_tcp","rdma"]}]}"#).unwrap());
    let src = rdma_fixture_with_config(runtime("copy"), c.clone());
    let dst = rdma_fixture_with_config(runtime("copy"), c);
    let remote = src
        .register_memory(
            vec![3, 1, 2, 3, 4, 5, 6, 7, 8, 4].into(),
            Permission::RemoteReadWrite,
        )
        .unwrap();
    let local = dst
        .register_memory(vec![0xcc; 10].into(), Permission::LocalOnly)
        .unwrap();
    let mut peer = dst
        .open_segment(src.endpoint(), src.source_epoch(), WAIT)
        .unwrap();
    for len in [8, 4] {
        // Keep a non-listening socket at this address so no unrelated service can take it.
        let guard = tokio::net::TcpSocket::new_v4().unwrap();
        guard.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        peer.metadata.hp = Some(Arc::new(
            Endpoint::new(
                guard.local_addr().unwrap().port(),
                8,
                &[Ipv4Addr::LOCALHOST],
            )
            .unwrap(),
        ));
        local.write(0, &[0xcc; 10]).unwrap();
        let req = TransferRequest::new(
            Transport::Unspec,
            local.base_address() + 1,
            remote.base_address() + 1,
            len,
        )
        .with_policy_name("host")
        .unwrap();
        let result = dst
            .submit_and_wait(TransferOp::Read, &peer, &[req], WAIT)
            .unwrap();
        assert!(result.is_complete(), "{result:?}");
        let mut want = vec![0xcc; 10];
        want[1..1 + len].copy_from_slice(&bytes(&remote)[1..1 + len]);
        assert_eq!(bytes(&local), want);
        assert_eq!(bytes(&remote), [3, 1, 2, 3, 4, 5, 6, 7, 8, 4]);
    }
}

#[cfg(feature = "metrics")]
#[test]
fn hp_tcp_bytes_and_attempts_use_the_hp_tcp_metrics_label() {
    let mut c = cfg();
    c.metrics.enabled = true;
    c.metrics.report_interval_seconds = 0;
    let (src, dst, remote, local, peer) = setup(c, 12);
    let req = TransferRequest::new(
        Transport::HpTcp,
        local.base_address() + 1,
        remote.base_address() + 1,
        8,
    );
    assert!(
        dst.submit_and_wait(TransferOp::Read, &peer, &[req], WAIT)
            .unwrap()
            .is_complete()
    );
    let metrics = dst.metrics().unwrap().prometheus();
    assert!(metrics.contains("orbite_read_bytes_total{transport=\"hp_tcp\"} 8"));
    assert_eq!(
        bytes(&local),
        [
            0xee, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xee, 0xee, 0xee
        ]
    );
    src.close(WAIT).unwrap();
}
