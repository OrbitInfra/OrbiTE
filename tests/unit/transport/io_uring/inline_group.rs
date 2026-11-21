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
fn buffered_write_group_drains_only_the_consumed_prefix_and_preserves_gaps() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image = (0..32).collect::<Vec<u8>>();
    for ready in [true, false] {
        for limit in [None, Some(3)] {
            let (f, trace) = observed_at(&dir, &image, ready, false, false)?;
            warm(&f, &trace)?;
            trace.lock().unwrap().submit_limit = limit;
            let requests = (0..8)
                .map(|i| f.request(i * 2, (i * 3) as u64, 1))
                .collect::<Vec<_>>();
            let workers = WORKER_STARTS.with(|n| n.get());
            let mut batch =
                f.engine
                    .submit_async(TransferOp::Write, &f.segment, &requests, None, TIMEOUT)?;
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
            let mut expected = image.clone();
            for i in 0..8 {
                let task = batch.task_status(i)?;
                assert!(task.locally_drained);
                if i < count {
                    assert_eq!(
                        (task.state, task.transferred_bytes),
                        (TaskState::Completed, 1)
                    );
                    expected[i * 3] = 0x9a;
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
            assert_eq!(fs::read(&f.path)?, expected);
            let t = trace.lock().unwrap();
            assert_eq!(t.group_checks.len(), 1);
            assert_eq!(t.group_checks[0].0 as usize, count);
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
fn lost_group_worker_retains_only_native_effects_for_the_consumed_prefix() -> Result<()> {
    let Some(dir) = directory("ORBITE_TMPFS_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let (f, trace) = observed_at(&dir, b"ABCDEFGH", false, false, false)?;
    warm(&f, &trace)?;
    trace.lock().unwrap().submit_limit = Some(2);
    FAIL_WORKER_START.with(|f| f.set(true));
    let requests = [f.request(0, 0, 1), f.request(2, 2, 1), f.request(4, 4, 1)];
    let batch = f
        .engine
        .submit_async(TransferOp::Write, &f.segment, &requests, None, TIMEOUT)?;
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
        assert_eq!(failure.remote_effect, RemoteEffect::Unknown);
    }
    let tail = batch.task_status(2)?;
    assert!(tail.locally_drained);
    assert_eq!(tail.state, TaskState::Skipped);
    assert_eq!(
        fs::read(&f.path)?,
        [0x9a, b'B', 0x9a, b'D', b'E', b'F', b'G', b'H']
    );
    assert!(matches!(
        f.engine.close_file_segment(&f.segment, Duration::ZERO),
        Err(Error::Timeout)
    ));
    drop(batch);
    assert!(trace.lock().unwrap().alive.upgrade().is_some());
    assert!(f.engine.stats()?.inflight_bytes >= 2);
    Ok(())
}

#[test]
fn group_caller_excludes_cold_direct_read_and_oversized_work() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for (var, op, count, size, cold) in [
        ("ORBITE_EXT4_DIR", TransferOp::Write, 2, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 2, 1, true),
        ("ORBITE_TMPFS_DIR", TransferOp::Read, 2, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 9, 1, false),
        ("ORBITE_TMPFS_DIR", TransferOp::Write, 8, 8193, false),
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
