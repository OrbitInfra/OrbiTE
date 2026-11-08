use super::*;

const WAIT: Duration = common::EVENTUAL_PROGRESS;

fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = [0; REQUEST_SIZE];
    stream.read_exact(&mut bytes).unwrap();
    Request::decode(&bytes).unwrap()
}
fn reply(stream: &mut TcpStream, epoch: SourceEpoch, status: u8, length: u64) {
    stream
        .write_all(
            &Reply {
                status,
                epoch,
                length,
            }
            .encode(),
        )
        .unwrap();
}
fn accept(listener: &TcpListener) -> TcpStream {
    let stream = listener.accept().unwrap().0;
    stream.set_nodelay(true).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    stream
}

#[test]
fn scattered_requests_reuse_one_stream_keep_write_order_and_guards() {
    let engine = TransferEngine::new(EngineConfig::default()).unwrap();
    let (listener, mut peer) = fault_peer();
    engine.bind_test_peer(&mut peer).unwrap();
    let epoch = peer.epoch;
    let handler = thread::spawn(move || {
        let mut stream = accept(&listener);
        for operation in [Operation::TcpWrite, Operation::TcpRead] {
            for index in 0..64u8 {
                let request = read_request(&mut stream);
                assert!(request.operation == operation);
                assert_eq!(
                    (request.epoch, request.offset, request.length),
                    (epoch, 0, 8)
                );
                if operation == Operation::TcpWrite {
                    reply(&mut stream, epoch, 9, 0);
                    let mut bytes = [0; 8];
                    stream.read_exact(&mut bytes).unwrap();
                    assert_eq!(bytes, [index; 8]);
                    reply(&mut stream, epoch, 0, 0);
                } else {
                    reply(&mut stream, epoch, 0, 8);
                    stream.write_all(&[index; 8]).unwrap();
                }
            }
        }
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(listener.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
    });
    let mut initial = vec![0xcd; 640];
    for i in 0..64 {
        initial[i * 10 + 1..i * 10 + 9].fill(i as u8);
    }
    let local = engine
        .register_memory(initial.clone().into(), Permission::LocalOnly)
        .unwrap();
    let requests: Vec<_> = (0..64)
        .map(|i| {
            TransferRequest::new(
                crate::Transport::Tcp,
                local.base_address() + i * 10 + 1,
                peer.regions[0].base_address(),
                8,
            )
        })
        .collect();
    for operation in [TransferOp::Write, TransferOp::Read] {
        if operation == TransferOp::Read {
            local.write(0, &[0xcd; 640]).unwrap();
        }
        let report = engine
            .submit_and_wait(operation, &peer, &requests, WAIT)
            .unwrap();
        assert!(report.is_complete());
        assert_eq!(
            (report.completed_requests, report.transferred_bytes),
            (64, 512)
        );
        let mut got = vec![0; 640];
        local.read(0, &mut got).unwrap();
        assert_eq!(got, initial);
    }
    engine
        .close_segment(peer.endpoint, peer.epoch, WAIT)
        .unwrap();
    assert!(matches!(
        engine.submit_and_wait(TransferOp::Read, &peer, &requests, WAIT),
        Err(Error::PeerClosed)
    ));
    handler.join().unwrap();
}

#[test]
fn partial_read_and_lost_write_ack_retire_the_stream_without_replay() {
    for operation in [TransferOp::Read, TransferOp::Write] {
        let engine = TransferEngine::new(EngineConfig::default()).unwrap();
        let (listener, mut peer) = fault_peer();
        engine.bind_test_peer(&mut peer).unwrap();
        let epoch = peer.epoch;
        let (failed_tx, failed_rx) = mpsc::channel();
        let (checked_tx, checked_rx) = mpsc::channel();
        let handler = thread::spawn(move || {
            let mut first = accept(&listener);
            read_request(&mut first);
            if operation == TransferOp::Read {
                reply(&mut first, epoch, 0, 8);
                first.write_all(&[1, 2, 3]).unwrap();
            } else {
                reply(&mut first, epoch, 9, 0);
                let mut body = [0; 8];
                first.read_exact(&mut body).unwrap();
                assert_eq!(body, [0x6d; 8]);
            }
            first.shutdown(Shutdown::Write).unwrap();
            assert_eq!(first.read(&mut [0]).unwrap(), 0);
            failed_rx.recv_timeout(WAIT).unwrap();
            // No policy walk or later request may replay the failed WRITE.
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
            listener.set_nonblocking(false).unwrap();
            checked_tx.send(()).unwrap();
            let mut next = accept(&listener);
            read_request(&mut next);
            reply(&mut next, epoch, 0, 8);
            next.write_all(&[0x82; 8]).unwrap();
            assert_eq!(next.read(&mut [0]).unwrap(), 0);
        });
        let local = engine
            .register_memory(vec![0x6d; 10].into(), Permission::LocalOnly)
            .unwrap();
        let request = TransferRequest::new(
            crate::Transport::Tcp,
            local.base_address() + 1,
            peer.regions[0].base_address(),
            8,
        );
        let report = engine
            .submit_and_wait(operation, &peer, &[request.clone(), request.clone()], WAIT)
            .unwrap();
        assert_eq!(report.completed_requests, 0);
        assert_eq!(
            report.failure.unwrap().remote_effect,
            if operation == TransferOp::Write {
                RemoteEffect::Unknown
            } else {
                RemoteEffect::None
            }
        );
        let mut bytes = [0; 10];
        local.read(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0x6d; 10]);
        failed_tx.send(()).unwrap();
        // Wait for the no-replay observation before an explicit fresh request.
        checked_rx.recv_timeout(WAIT).unwrap();
        assert!(
            engine
                .submit_and_wait(TransferOp::Read, &peer, &[request], WAIT)
                .unwrap()
                .is_complete()
        );
        local.read(0, &mut bytes).unwrap();
        assert_eq!(
            bytes,
            [0x6d, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82, 0x6d]
        );
        engine
            .close_segment(peer.endpoint, peer.epoch, WAIT)
            .unwrap();
        handler.join().unwrap();
    }
}

#[test]
fn idle_tcp_stream_does_not_hold_source_admission_or_a_handler() {
    let config = EngineConfig {
        handler_threads: 1,
        ..EngineConfig::default()
    };
    let source = TransferEngine::new(config).unwrap();
    let memory = source
        .register_memory(vec![0x72; 10].into(), Permission::RemoteRead)
        .unwrap();
    let a = TransferEngine::new(EngineConfig::default()).unwrap();
    let b = TransferEngine::new(EngineConfig::default()).unwrap();
    for engine in [&a, &b] {
        let peer = engine
            .open_segment(source.endpoint(), source.source_epoch(), WAIT)
            .unwrap();
        let local = engine
            .register_memory(vec![0xcd; 10].into(), Permission::LocalOnly)
            .unwrap();
        let request = TransferRequest::new(
            crate::Transport::Tcp,
            local.base_address() + 1,
            memory.base_address() + 1,
            8,
        );
        assert!(
            engine
                .submit_and_wait(TransferOp::Read, &peer, &[request], WAIT)
                .unwrap()
                .is_complete()
        );
        let mut got = [0; 10];
        local.read(0, &mut got).unwrap();
        assert_eq!(
            got,
            [0xcd, 0x72, 0x72, 0x72, 0x72, 0x72, 0x72, 0x72, 0x72, 0xcd]
        );
    }
    // Neither cached stream pins the source registration or its admission.
    source.unregister_memory(&memory).unwrap();
    source.close(WAIT).unwrap();
    assert_eq!(source.stats().unwrap().inflight_bytes, 0);
}
