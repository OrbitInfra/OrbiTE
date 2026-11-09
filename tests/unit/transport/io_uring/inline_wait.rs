use super::*;

#[test]
fn caller_wait_is_bounded_by_one_window_the_request_deadline_and_three_enters() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for budget in [Duration::ZERO, SMALL_WAIT / 2, TIMEOUT] {
        let trace = Arc::new(Mutex::new(Trace {
            submitter: Some(std::thread::current().id()),
            errors: [libc::EINTR; 4].into(),
            ..Default::default()
        }));
        // No SQEs are published: inject interrupted enters at the same boundary
        // as the real ring, without scheduling a payload or a worker.
        let live = LiveBatch {
            ring: Box::new(Observed {
                ring: ring::create(1)?,
                trace: trace.clone(),
                _lifetime: Arc::new(()),
                receipt: None,
                ready: false,
                no_space: false,
            }),
            tasks: vec![],
            _iovecs: vec![],
        };
        let start = Instant::now();
        let deadline = start + budget;
        let mut owner = Submitted {
            live: ManuallyDrop::new(live),
            drain: Drain {
                consumed: 0,
                submit_error: 0,
                status: Ok(()),
            },
            deadline,
            drain_deadline: deadline + DESTRUCTION_TIMEOUT,
        };
        assert!(owner.wait_small().is_none());
        let after = Instant::now();
        let t = trace.lock().unwrap();
        assert!(t.waits.len() <= 3);
        assert!(
            t.waits
                .iter()
                .all(|until| *until <= deadline && *until <= after + SMALL_WAIT)
        );
        assert!(t.waits.windows(2).all(|pair| pair[0] == pair[1]));
        if budget.is_zero() {
            assert!(t.waits.is_empty());
        } else if after - start < budget.min(SMALL_WAIT) {
            assert_eq!(t.waits.len(), 3);
        }
        drop(ManuallyDrop::into_inner(owner.live));
    }
    Ok(())
}

#[test]
fn late_small_cqe_waits_on_caller_without_a_worker_on_tmpfs_and_direct_ext4() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for (variable, direct) in [("ORBITE_TMPFS_DIR", false), ("ORBITE_EXT4_DIR", true)] {
        let Some(directory) = std::env::var_os(variable) else {
            assert!(!required(), "{variable} must select the real filesystem");
            eprintln!("skipping {variable} real-file control: directory not configured");
            continue;
        };
        for op in [TransferOp::Read, TransferOp::Write] {
            let image: Vec<_> = (0..12288).map(|i| (i % 251) as u8).collect();
            let (f, trace) = observed_at(Path::new(&directory), &image, false, false, false)?;
            trace.lock().unwrap().wait_ready = true;
            let before_workers = WORKER_STARTS.with(|n| n.get());
            let before = Instant::now();
            let mut batch = f.engine.submit_async(
                op,
                &f.segment,
                &[f.request(1, 4096, 4096)],
                None,
                TIMEOUT,
            )?;
            let after = Instant::now();
            assert_eq!(WORKER_STARTS.with(|n| n.get()), before_workers);
            assert_eq!(f.engine.stats()?.requester_workers, 0);
            assert_eq!(batch.wait(TIMEOUT)?.transferred_bytes, 4096);
            assert!(batch.locally_drained()?);
            let t = trace.lock().unwrap();
            assert_eq!((t.submits, t.peeks, t.reaps, t.waits.len()), (1, 1, 1, 1));
            assert!((before..=after + SMALL_WAIT).contains(&t.waits[0]));
            assert!(t.alive.upgrade().is_none());
            drop(t);
            assert_eq!(
                f.engine.file_segment_info(&f.segment)?.direct_io,
                Some(direct)
            );
            let mut expected = vec![0x9a; f.memory.len()];
            let mut file = image.clone();
            if op == TransferOp::Read {
                expected[1..4097].copy_from_slice(&image[4096..8192]);
            } else {
                file[4096..8192].fill(0x9a);
            }
            let mut actual = vec![0; f.memory.len()];
            f.memory.read(0, &mut actual)?;
            assert_eq!(actual, expected);
            assert_eq!(fs::read(&f.path)?, file);
            batch.free()?;
        }
    }
    Ok(())
}

#[test]
fn caller_wait_interruptions_errors_and_short_cqe_never_resubmit() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for (errors, short) in [
        (vec![libc::EINTR, libc::EAGAIN], false),
        (vec![libc::EINTR; 4], false),
        (vec![libc::ETIME], false),
        (vec![libc::EIO], false),
        (vec![], true),
    ] {
        for op in [TransferOp::Read, TransferOp::Write] {
            if short && op == TransferOp::Write {
                continue; // Real EOF produces the short READ; no fabricated prefix.
            }
            let (f, trace) = observed(false, short, false)?;
            {
                let mut t = trace.lock().unwrap();
                t.wait_ready = true;
                t.errors = errors.clone().into();
            }
            let mut batch =
                f.engine
                    .submit_async(op, &f.segment, &[f.request(1, 0, 8)], None, TIMEOUT)?;
            let status = batch.wait(TIMEOUT)?;
            assert!(status.locally_drained);
            let task = batch.task_status(0)?;
            assert_eq!(task.transferred_bytes, if short { 3 } else { 8 });
            assert_eq!(
                task.state,
                if short {
                    TaskState::Failed
                } else {
                    TaskState::Completed
                }
            );
            let reports = batch.submissions()?;
            assert_eq!(reports[0].transferred_bytes, if short { 0 } else { 8 });
            if short {
                assert!(
                    matches!(&task.failure.unwrap().error, Error::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof)
                );
            }
            let t = trace.lock().unwrap();
            assert_eq!(t.submits, 1);
            assert!(!t.waits.is_empty() && t.waits.len() <= 3);
            assert!(t.waits.iter().all(|until| *until == t.waits[0]));
            assert!(t.alive.upgrade().is_none());
            drop(t);
            let mut expected = vec![0x9a; f.memory.len()];
            if op == TransferOp::Read {
                let bytes: &[u8] = if short { b"abc" } else { b"abcdefgh" };
                expected[1..1 + bytes.len()].copy_from_slice(bytes);
            }
            let mut actual = vec![0; f.memory.len()];
            f.memory.read(0, &mut actual)?;
            assert_eq!(actual, expected);
            assert_eq!(
                fs::read(&f.path)?,
                if op == TransferOp::Write {
                    vec![0x9a; 8]
                } else if short {
                    b"abc".to_vec()
                } else {
                    b"abcdefgh".to_vec()
                }
            );
            batch.free()?;
        }
    }
    Ok(())
}

#[test]
fn expired_caller_window_drop_and_engine_close_keep_the_drain_owner() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for op in [TransferOp::Read, TransferOp::Write] {
        let (f, started, release) = held_file(EngineConfig::default())?;
        let batch = f
            .engine
            .submit_async(op, &f.segment, &[f.request(1, 0, 4)], None, TIMEOUT)?;
        let receipt = batch.allocation_release_receipt();
        let wait = started.recv_timeout(EVENTUAL_PROGRESS).unwrap();
        drop(batch);
        assert!(matches!(
            f.engine.close(Duration::ZERO),
            Err(Error::Timeout)
        ));
        assert!(wait.ring.upgrade().is_some());
        assert_eq!(f.engine.stats()?.inflight_bytes, 4);
        assert!(receipt.try_recv().is_err());
        assert!(f.memory.write(1, b"x").is_err());
        release.send(()).unwrap();
        receipt.recv_timeout(EVENTUAL_PROGRESS).unwrap();
        assert!(wait.ring.upgrade().is_none());
        assert_eq!(f.engine.stats()?.inflight_bytes, 0);
        f.engine.close(TIMEOUT)?;
        let mut expected = vec![0x9a; f.memory.len()];
        if op == TransferOp::Read {
            expected[1..5].copy_from_slice(b"1234");
        }
        let mut actual = vec![0; f.memory.len()];
        f.memory.read(0, &mut actual)?;
        assert_eq!(actual, expected);
        assert_eq!(
            fs::read(&f.path)?,
            if op == TransferOp::Write {
                [vec![0x9a; 4], b"5678".to_vec()].concat()
            } else {
                b"12345678".to_vec()
            }
        );
    }
    Ok(())
}

#[test]
fn cancel_racing_the_late_receipt_keeps_bytes_and_retires_once() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for op in [TransferOp::Read, TransferOp::Write] {
        let (f, started, release) = held_file(EngineConfig::default())?;
        let mut batch =
            f.engine
                .submit_async(op, &f.segment, &[f.request(1, 0, 4)], None, TIMEOUT)?;
        let wait = started.recv_timeout(EVENTUAL_PROGRESS).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                release.send(()).unwrap();
            });
            barrier.wait();
            // Both orders are allowed; accepted cancel still cannot undo the
            // posted operation or turn cancellation into physical retirement.
            batch.cancel(0).unwrap();
        });
        let status = batch.wait(TIMEOUT)?;
        assert_eq!((status.completed_tasks, status.transferred_bytes), (1, 4));
        assert!(status.locally_drained);
        batch.free()?;
        assert!(wait.ring.upgrade().is_none());
        assert_eq!(f.engine.stats()?.inflight_bytes, 0);
        let mut expected = vec![0x9a; f.memory.len()];
        if op == TransferOp::Read {
            expected[1..5].copy_from_slice(b"1234");
        }
        let mut actual = vec![0; f.memory.len()];
        f.memory.read(0, &mut actual)?;
        assert_eq!(actual, expected);
        assert_eq!(
            fs::read(&f.path)?,
            if op == TransferOp::Write {
                [vec![0x9a; 4], b"5678".to_vec()].concat()
            } else {
                b"12345678".to_vec()
            }
        );
    }
    Ok(())
}
