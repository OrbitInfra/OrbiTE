use super::*;

fn directory(variable: &str) -> Option<PathBuf> {
    let path = std::env::var_os(variable).map(PathBuf::from);
    if path.is_none() {
        assert!(!required(), "{variable} must select the real filesystem");
        eprintln!("skipping real file control without {variable}");
    }
    path
}

fn warm(f: &Fixture, trace: &Mutex<Trace>) -> Result<()> {
    trace.lock().unwrap().wait_ready = true;
    assert!(
        f.run(TransferOp::Read, &[f.request(128, 0, 1)])?
            .is_complete()
    );
    *trace.lock().unwrap() = Trace::default();
    Ok(())
}

#[test]
fn buffered_group_drains_only_the_consumed_prefix_and_preserves_gaps() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image = (0..32).collect::<Vec<u8>>();
    for (op, ready) in [
        (TransferOp::Write, true),
        (TransferOp::Write, false),
        (TransferOp::Read, true),
        (TransferOp::Read, false),
    ] {
        for limit in [None, Some(3), Some(1)] {
            let (f, trace) = observed_at(&dir, &image, ready, false, false)?;
            warm(&f, &trace)?;
            let mut expected_memory = vec![0; f.memory.len()];
            f.memory.read(0, &mut expected_memory)?;
            trace.lock().unwrap().submit_limit = limit;
            let requests = (0..8)
                .map(|i| f.request(i * 2, (i * 3) as u64, 1))
                .collect::<Vec<_>>();
            let workers = WORKER_STARTS.with(|n| n.get());
            let mut batch = f
                .engine
                .submit_async(op, &f.segment, &requests, None, TIMEOUT)?;
            let status = batch.wait(TIMEOUT)?;
            let count = limit.unwrap_or(8) as usize;
            assert_eq!(
                WORKER_STARTS.with(|n| n.get()) - workers,
                usize::from(!ready)
            );
            assert_eq!(
                (
                    status.completed_tasks,
                    status.transferred_bytes,
                    status.locally_drained
                ),
                (count, count, true)
            );
            let mut expected_file = image.clone();
            for i in 0..8 {
                let task = batch.task_status(i)?;
                assert!(task.locally_drained);
                if i < count {
                    assert_eq!(
                        (task.state, task.transferred_bytes),
                        (TaskState::Completed, 1)
                    );
                    match op {
                        TransferOp::Read => expected_memory[i * 2] = image[i * 3],
                        TransferOp::Write => expected_file[i * 3] = expected_memory[i * 2],
                    }
                } else {
                    let failure = task.failure.unwrap();
                    assert_eq!(failure.remote_effect, RemoteEffect::None);
                    assert!(matches!(
                        failure.error,
                        Error::Backend {
                            operation: "io_uring submit",
                            ..
                        }
                    ));
                }
            }
            assert_eq!(fs::read(&f.path)?, expected_file);
            let mut bytes = vec![0; f.memory.len()];
            f.memory.read(0, &mut bytes)?;
            assert_eq!(bytes, expected_memory);
            let t = trace.lock().unwrap();
            let checked: Vec<_> = t.group_checks.iter().map(|(n, _)| *n as usize).collect();
            if count == 1 {
                assert!(checked.is_empty());
                assert_eq!(t.peeks, 1);
            } else {
                assert_eq!(checked, [count]);
            }
            assert_eq!(t.submitter, Some(std::thread::current().id()));
            assert_eq!(t.submits, if limit.is_some() { 2 } else { 1 });
            assert!(t.alive.upgrade().is_none());
            drop(t);
            batch.free()?;
            assert_eq!(f.engine.stats()?.inflight_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn short_buffered_read_group_preserves_each_cqe_and_destination_guard() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    for ready in [true, false] {
        let (f, trace) = observed_at(&dir, b"abcdefgh", ready, true, false)?;
        warm(&f, &trace)?;
        // Restore the advertised range for admission. The ring factory
        // truncates it to three bytes after the requests have been prepared.
        fs::write(&f.path, b"abcdefgh")?;
        let requests = [f.request(1, 1, 4), f.request(9, 4, 2), f.request(17, 0, 3)];
        let workers = WORKER_STARTS.with(|n| n.get());
        let mut batch =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &requests, None, TIMEOUT)?;
        let status = batch.wait(TIMEOUT)?;
        assert!(status.locally_drained);
        assert_eq!((status.completed_tasks, status.transferred_bytes), (1, 5));
        let tasks = batch.task_statuses()?;
        for (task, (state, bytes)) in tasks.iter().zip([
            (TaskState::Failed, 2),
            (TaskState::Failed, 0),
            (TaskState::Completed, 3),
        ]) {
            assert_eq!(
                (task.state, task.transferred_bytes, task.locally_drained),
                (state, bytes, true)
            );
        }
        assert_eq!(
            WORKER_STARTS.with(|n| n.get()) - workers,
            usize::from(!ready)
        );
        assert_eq!(trace.lock().unwrap().submits, 1);
        let mut bytes = [0; 24];
        f.memory.read(0, &mut bytes)?;
        let mut expected = [0x9a; 24];
        expected[1..3].copy_from_slice(b"bc");
        expected[17..20].copy_from_slice(b"abc");
        assert_eq!(bytes, expected);
        batch.free()?;
        assert_eq!(f.engine.stats()?.inflight_bytes, 0);
    }
    Ok(())
}

#[test]
fn lost_group_worker_retains_only_native_effects_for_the_consumed_prefix() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    for op in [TransferOp::Read, TransferOp::Write] {
        check_lost_group_worker(&dir, op)?;
    }
    Ok(())
}

fn check_lost_group_worker(dir: &Path, op: TransferOp) -> Result<()> {
    let (f, trace) = observed_at(dir, b"ABCDEFGH", false, false, false)?;
    warm(&f, &trace)?;
    trace.lock().unwrap().submit_limit = Some(2);
    FAIL_WORKER_START.with(|f| f.set(true));
    let requests = [f.request(0, 0, 1), f.request(2, 2, 1), f.request(4, 4, 1)];
    let batch = f
        .engine
        .submit_async(op, &f.segment, &requests, None, TIMEOUT)?;
    assert!(!batch.wait(TIMEOUT)?.locally_drained);
    for i in 0..2 {
        let task = batch.task_status(i)?;
        assert!(!task.locally_drained);
        let failure = task.failure.unwrap();
        assert!(matches!(
            failure.error,
            Error::Unquiesced {
                operation: "io_uring worker start",
                ..
            }
        ));
        assert_eq!(
            failure.remote_effect,
            match op {
                TransferOp::Read => RemoteEffect::None,
                TransferOp::Write => RemoteEffect::Unknown,
            }
        );
        assert!(matches!(
            f.memory.read(i * 2, &mut [0]),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            f.memory.write(i * 2, &[0]),
            Err(Error::Unavailable(_))
        ));
    }
    let tail = batch.task_status(2)?;
    assert!(tail.locally_drained);
    assert_eq!(tail.state, TaskState::Skipped);
    let mut byte = [0];
    f.memory.read(4, &mut byte)?;
    assert_eq!(byte, [0x9a]);
    f.memory.write(4, &[0x42])?;
    f.memory.read(4, &mut byte)?;
    assert_eq!(byte, [0x42]);
    let expected = match op {
        TransferOp::Read => *b"ABCDEFGH",
        TransferOp::Write => [0x9a, b'B', 0x9a, b'D', b'E', b'F', b'G', b'H'],
    };
    assert_eq!(fs::read(&f.path)?, expected);
    assert!(matches!(
        f.engine.close_file_segment(&f.segment, Duration::ZERO),
        Err(Error::Timeout)
    ));
    drop(batch);
    assert!(trace.lock().unwrap().alive.upgrade().is_some());
    assert_eq!(f.engine.stats()?.inflight_bytes, 2);
    Ok(())
}

#[test]
fn invalid_ring_receipts_keep_the_consumed_ranges_and_charges() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    for invalid_sq_head in [false, true] {
        let (f, trace) = observed_at(&dir, b"ABCDEFGH", true, false, false)?;
        warm(&f, &trace)?;
        {
            let mut t = trace.lock().unwrap();
            t.submit_limit = Some(2);
            t.invalid_sq_head = invalid_sq_head;
            t.invalid_cqe = !invalid_sq_head;
        }
        let requests = [f.request(0, 0, 1), f.request(2, 2, 1), f.request(4, 4, 1)];
        let batch =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &requests, None, TIMEOUT)?;
        assert!(!batch.wait(TIMEOUT)?.locally_drained);
        let retained = if invalid_sq_head { 3 } else { 2 };
        for i in 0..retained {
            assert!(!batch.task_status(i)?.locally_drained);
            assert!(matches!(
                f.memory.read(2 * i, &mut [0]),
                Err(Error::Unavailable(_))
            ));
        }
        if !invalid_sq_head {
            assert!(batch.task_status(2)?.locally_drained);
            f.memory.write(4, &[0x42])?;
            let mut byte = [0];
            f.memory.read(4, &mut byte)?;
            assert_eq!(byte, [0x42]);
        }
        assert_eq!(f.engine.stats()?.inflight_bytes, retained);
    }
    Ok(())
}

#[test]
fn group_caller_excludes_cold_direct_and_oversized_work() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for (var, op, count, size, cold) in [
        ("ORBITE_EXT4_DIR", TransferOp::Write, 2, 1, false),
        ("ORBITE_EXT4_DIR", TransferOp::Read, 2, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 2, 1, true),
        ("ORBITE_TMPFS_DIR", TransferOp::Read, 2, 1, true),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 9, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 8, 8193, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Read, 9, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Read, 8, 8193, false),
    ] {
        let Some(dir) = directory(var) else { continue };
        let span = count * (size + 1);
        let (mut f, trace) = observed_at(&dir, &vec![0; span], true, false, false)?;
        if span > f.memory.len() {
            f.memory = f
                .engine
                .register_memory(vec![0x9a; span].into_boxed_slice(), Permission::LocalOnly)?;
        }
        if !cold {
            warm(&f, &trace)?;
        }
        let requests = (0..count)
            .map(|i| {
                let at = i * (size + 1);
                f.request(at, at as u64, size)
            })
            .collect::<Vec<_>>();
        let workers = WORKER_STARTS.with(|n| n.get());
        let mut batch = f
            .engine
            .submit_async(op, &f.segment, &requests, None, TIMEOUT)?;
        assert_eq!(batch.wait(TIMEOUT)?.completed_tasks, count);
        assert_eq!(WORKER_STARTS.with(|n| n.get()) - workers, 1);
        let t = trace.lock().unwrap();
        assert_ne!(t.submitter, Some(std::thread::current().id()));
        assert!(t.group_checks.is_empty());
        drop(t);
        batch.free()?;
    }
    Ok(())
}
