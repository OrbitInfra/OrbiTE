use super::*;

#[derive(Default)]
struct Trace {
    submitter: Option<std::thread::ThreadId>,
    submits: usize,
    peeks: usize,
    reaps: usize,
    waits: Vec<Instant>,
    wait_ready: bool,
    errors: std::collections::VecDeque<i32>,
    alive: std::sync::Weak<()>,
}
struct Observed {
    ring: Box<dyn Ring>,
    trace: Arc<Mutex<Trace>>,
    _lifetime: Arc<()>,
    receipt: Option<(u64, i32)>,
    ready: bool,
    no_space: bool,
}
impl Ring for Observed {
    fn space(&mut self) -> usize {
        if self.no_space { 0 } else { self.ring.space() }
    }
    fn queued(&mut self) -> usize {
        self.ring.queued()
    }
    fn push(&mut self, entries: &[squeue::Entry]) -> io::Result<()> {
        self.ring.push(entries)
    }
    fn submit(&mut self, count: u32) -> io::Result<usize> {
        let mut trace = self.trace.lock().unwrap();
        trace.submits += 1;
        trace.submitter = Some(std::thread::current().id());
        drop(trace);
        let n = self.ring.submit(count)?;
        if n == 1 {
            // Hold a real kernel receipt to make caller completion deterministic.
            // No fabricated CQE and no sleep controls the path under test.
            self.receipt = Some(self.ring.reap(Instant::now() + EVENTUAL_PROGRESS)?);
        }
        Ok(n)
    }
    fn peek(&mut self) -> io::Result<Option<(u64, i32)>> {
        self.trace.lock().unwrap().peeks += 1;
        Ok(if self.ready {
            self.receipt.take()
        } else {
            None
        })
    }
    fn reap(&mut self, until: Instant) -> io::Result<(u64, i32)> {
        let mut t = self.trace.lock().unwrap();
        t.reaps += 1;
        if t.submitter == Some(std::thread::current().id()) {
            t.waits.push(until);
            if let Some(code) = t.errors.pop_front() {
                return Err(io::Error::from_raw_os_error(code));
            }
            if !self.ready && !t.wait_ready {
                return Err(io::Error::from_raw_os_error(libc::ETIME));
            }
        }
        drop(t);
        self.receipt
            .take()
            .map_or_else(|| self.ring.reap(until), Ok)
    }
}
fn observed(ready: bool, truncate: bool, no_space: bool) -> Result<(Fixture, Arc<Mutex<Trace>>)> {
    observed_at(
        &std::env::temp_dir(),
        b"abcdefgh",
        ready,
        truncate,
        no_space,
    )
}

fn observed_at(
    directory: &Path,
    image: &[u8],
    ready: bool,
    truncate: bool,
    no_space: bool,
) -> Result<(Fixture, Arc<Mutex<Trace>>)> {
    let mut f = Fixture::new(directory, image, TransferEngine::new(Default::default())?)?;
    let path = f.path.clone();
    let trace = Arc::new(Mutex::new(Trace::default()));
    let copy = trace.clone();
    let io = Arc::get_mut(&mut f.engine)
        .unwrap()
        .io_uring
        .as_mut()
        .unwrap();
    io.available = Arc::new(true.into());
    io.create = Arc::new(move |count| {
        if truncate {
            fs::OpenOptions::new().write(true).open(&path)?.set_len(3)?;
        }
        let alive = Arc::new(());
        copy.lock().unwrap().alive = Arc::downgrade(&alive);
        Ok(Box::new(Observed {
            ring: ring::create(count)?,
            trace: copy.clone(),
            _lifetime: alive,
            receipt: None,
            ready,
            no_space,
        }))
    });
    Ok((f, trace))
}

#[path = "inline_wait.rs"]
mod wait;

#[test]
fn ready_small_file_cqes_finish_on_the_caller_with_exact_prefix_and_guards() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for (op, short) in [
        (TransferOp::Read, false),
        (TransferOp::Write, false),
        (TransferOp::Read, true),
    ] {
        let (f, trace) = observed(true, short, false)?;
        let mut batch = f.engine.allocate_batch(2)?;
        // Also exercise a second append after an inline receipt, on the same owner.
        for at in [1, 17] {
            assert_eq!(
                batch
                    .submit(op, &f.segment, &[f.request(at, 0, 8)], None, TIMEOUT)?
                    .len(),
                1
            );
            assert_eq!(f.engine.stats()?.requester_workers, 0);
            assert_eq!(f.engine.stats()?.inflight_bytes, 0);
            let t = trace.lock().unwrap();
            assert_eq!(t.submitter, Some(std::thread::current().id()));
            assert_eq!(t.reaps, 0);
            assert!(t.alive.upgrade().is_none());
            drop(t);
            let task = batch.task_status(if at == 1 { 0 } else { 1 })?;
            assert!(task.locally_drained);
            assert_eq!(task.transferred_bytes, if short { 3 } else { 8 });
            assert_eq!(
                task.state,
                if short {
                    TaskState::Failed
                } else {
                    TaskState::Completed
                }
            );
            if short {
                assert!(
                    matches!(&task.failure.unwrap().error, Error::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof)
                );
            }
            // Restore the advertised extent before the next admitted short READ.
            if short {
                fs::write(&f.path, b"abcdefgh")?;
            }
        }
        batch.seal()?;
        assert_eq!(
            batch.wait(TIMEOUT)?.completed_tasks,
            if short { 0 } else { 2 }
        );
        let groups = batch.submissions()?;
        for group in groups {
            assert_eq!(group.completed_requests, usize::from(!short));
            assert_eq!(group.transferred_bytes, if short { 0 } else { 8 });
        }
        let mut expected = vec![0x9a; f.memory.len()];
        if op == TransferOp::Read {
            for at in [1, 17] {
                expected[at..at + if short { 3 } else { 8 }].copy_from_slice(if short {
                    b"abc"
                } else {
                    b"abcdefgh"
                });
            }
        }
        let mut bytes = vec![0; f.memory.len()];
        f.memory.read(0, &mut bytes)?;
        assert_eq!(bytes, expected);
        if op == TransferOp::Write {
            assert_eq!(fs::read(&f.path)?, vec![0x9a; 8]);
        }
        assert_eq!(trace.lock().unwrap().submits, 2);
        batch.free()?;
    }
    Ok(())
}

#[test]
fn pending_small_file_handoff_keeps_loans_through_cancel_timeout_and_close() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for op in [TransferOp::Read, TransferOp::Write] {
        let (f, started, release) = held_file(EngineConfig::default())?;
        let mut batch =
            f.engine
                .submit_async(op, &f.segment, &[f.request(1, 0, 4)], None, TIMEOUT)?;
        let held = started.recv_timeout(EVENTUAL_PROGRESS).unwrap();
        assert!(held.ring.upgrade().is_some());
        assert_eq!(f.engine.stats()?.inflight_bytes, 4);
        assert_eq!(f.engine.stats()?.requester_workers, 1);
        assert!(batch.cancel(0)?);
        assert!(matches!(batch.wait(Duration::ZERO), Err(Error::Timeout)));
        assert!(!batch.locally_drained()?);
        assert!(matches!(batch.free(), Err(Error::BatchBusy)));
        assert!(matches!(
            f.engine.close_file_segment(&f.segment, Duration::ZERO),
            Err(Error::Timeout)
        ));
        f.engine.unregister_memory(&f.memory)?;
        assert!(f.memory.read(1, &mut [0; 4]).is_err());
        assert_eq!(f.engine.stats()?.inflight_bytes, 4);
        release.send(()).unwrap();
        assert_eq!(batch.wait(TIMEOUT)?.completed_tasks, 1);
        batch.free()?;
        assert!(held.ring.upgrade().is_none());
        assert_eq!(f.engine.stats()?.inflight_bytes, 0);
        let mut expected = vec![0x9a; f.memory.len()];
        if op == TransferOp::Read {
            expected[1..5].copy_from_slice(b"1234");
        }
        let mut bytes = vec![0; f.memory.len()];
        f.memory.read(0, &mut bytes)?;
        assert_eq!(bytes, expected);
        assert_eq!(
            fs::read(&f.path)?,
            if op == TransferOp::Write {
                [&[0x9a; 4][..], &b"5678"[..]].concat()
            } else {
                b"12345678".to_vec()
            }
        );
        f.engine.close_file_segment(&f.segment, TIMEOUT)?;
    }
    Ok(())
}

#[test]
fn failed_worker_handoff_quarantines_the_accepted_sqe_without_replay() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for error in [libc::ETIME, libc::EIO] {
        let (f, trace) = observed(false, false, false)?;
        trace.lock().unwrap().errors.push_back(error);
        FAIL_WORKER_START.with(|f| f.set(true));
        let mut batch = f.engine.submit_async(
            TransferOp::Write,
            &f.segment,
            &[f.request(1, 0, 8)],
            None,
            TIMEOUT,
        )?;
        let status = batch.wait(TIMEOUT)?;
        assert!(!status.locally_drained);
        assert_eq!(status.completed_tasks, 0);
        let task = batch.task_status(0)?;
        let failure = task.failure.unwrap();
        assert!(matches!(
            failure.error,
            Error::Unquiesced {
                operation: "io_uring worker start",
                ..
            }
        ));
        assert_eq!(failure.remote_effect, RemoteEffect::Unknown);
        let t = trace.lock().unwrap();
        assert_eq!((t.submits, t.peeks, t.reaps), (1, 1, 1));
        assert!(t.alive.upgrade().is_some());
        drop(t);
        assert_eq!(f.engine.stats()?.inflight_bytes, 8);
        assert_eq!(f.engine.stats()?.requester_workers, 0);
        assert!(matches!(batch.free(), Err(Error::BatchBusy)));
        assert!(matches!(
            f.engine.close_file_segment(&f.segment, Duration::ZERO),
            Err(Error::Timeout)
        ));
        f.engine.unregister_memory(&f.memory)?;
        assert!(f.memory.read(1, &mut [0; 8]).is_err());
        // Actual kernel WRITE completed, but no release receipt reached the owner.
        assert_eq!(fs::read(&f.path)?, vec![0x9a; 8]);
        drop(batch);
        assert!(trace.lock().unwrap().alive.upgrade().is_some());
        assert_eq!(f.engine.stats()?.inflight_bytes, 8);
    }
    Ok(())
}

#[test]
fn caller_preflight_rejects_full_sq_before_any_file_io() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    let (f, trace) = observed(true, false, true)?;
    let mut batch = f.engine.submit_async(
        TransferOp::Write,
        &f.segment,
        &[f.request(1, 0, 8)],
        None,
        TIMEOUT,
    )?;
    let task = batch.task_status(0)?;
    assert!(matches!(
        task.failure.unwrap().error,
        Error::ResourceExhausted
    ));
    assert!(task.locally_drained);
    assert_eq!(trace.lock().unwrap().submits, 0);
    assert!(trace.lock().unwrap().alive.upgrade().is_none());
    assert_eq!(fs::read(&f.path)?, b"abcdefgh");
    let mut bytes = vec![0; f.memory.len()];
    f.memory.read(0, &mut bytes)?;
    assert_eq!(bytes, vec![0x9a; f.memory.len()]);
    assert_eq!(f.engine.stats()?.inflight_bytes, 0);
    assert_eq!(f.engine.stats()?.requester_workers, 0);
    batch.free()?;
    Ok(())
}

#[test]
fn aligned_small_file_requests_need_no_bounce_allocation() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    let (mut f, trace) = observed(true, false, false)?;
    let image: Vec<_> = (0..8192).map(|i| (i % 251) as u8).collect();
    fs::write(&f.path, &image)?;
    let io = Arc::get_mut(&mut f.engine)
        .unwrap()
        .io_uring
        .as_mut()
        .unwrap();
    io.allocate = |_, _| panic!("aligned small request allocated a bounce buffer");
    let base = f.memory.base_address();
    let at = (base.next_multiple_of(4096) - base) as usize;
    let request = [f.request(at, 4096, 4096)];
    let mut batch = f
        .engine
        .submit_async(TransferOp::Read, &f.segment, &request, None, TIMEOUT)?;
    assert_eq!(batch.wait(TIMEOUT)?.transferred_bytes, 4096);
    let mut expected = vec![0x9a; f.memory.len()];
    expected[at..at + 4096].copy_from_slice(&image[4096..]);
    let mut bytes = vec![0; f.memory.len()];
    f.memory.read(0, &mut bytes)?;
    assert_eq!(bytes, expected);
    assert_eq!(
        trace.lock().unwrap().submitter,
        Some(std::thread::current().id())
    );
    batch.free()?;
    Ok(())
}

#[test]
fn pending_native_io_completes_on_worker_while_caller_only_computes() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    struct AsyncHandoff {
        inner: Box<dyn Ring>,
        caller: std::thread::ThreadId,
        reaped: Arc<AtomicBool>,
    }
    impl Ring for AsyncHandoff {
        fn space(&mut self) -> usize {
            self.inner.space()
        }
        fn queued(&mut self) -> usize {
            self.inner.queued()
        }
        fn push(&mut self, entries: &[squeue::Entry]) -> io::Result<()> {
            let entries: Vec<_> = entries
                .iter()
                .cloned()
                .map(|entry| entry.flags(squeue::Flags::ASYNC))
                .collect();
            self.inner.push(&entries)
        }
        fn submit(&mut self, count: u32) -> io::Result<usize> {
            assert_eq!(std::thread::current().id(), self.caller);
            self.inner.submit(count)
        }
        fn peek(&mut self) -> io::Result<Option<(u64, i32)>> {
            Ok(None) // Force worker handoff even if the real CQE arrived early.
        }
        fn reap(&mut self, until: Instant) -> io::Result<(u64, i32)> {
            if std::thread::current().id() == self.caller {
                return Err(io::Error::from_raw_os_error(libc::ETIME));
            }
            assert_ne!(std::thread::current().id(), self.caller);
            let cqe = self.inner.reap(until)?;
            self.reaped.store(true, Ordering::Release);
            Ok(cqe)
        }
    }
    for op in [TransferOp::Read, TransferOp::Write] {
        let (mut f, _) = observed(false, false, false)?;
        let reaped = Arc::new(AtomicBool::new(false));
        let done = reaped.clone();
        let caller = std::thread::current().id();
        Arc::get_mut(&mut f.engine)
            .unwrap()
            .io_uring
            .as_mut()
            .unwrap()
            .create = Arc::new(move |count| {
            Ok(Box::new(AsyncHandoff {
                inner: ring::create(count)?,
                caller,
                reaped: done.clone(),
            }))
        });
        let mut batch =
            f.engine
                .submit_async(op, &f.segment, &[f.request(1, 0, 8)], None, TIMEOUT)?;
        let until = Instant::now() + EVENTUAL_PROGRESS;
        // No status polling or explicit syscall to drive the submitter's task work.
        while !reaped.load(Ordering::Acquire) && Instant::now() < until {
            std::hint::spin_loop();
        }
        assert!(reaped.load(Ordering::Acquire));
        assert_eq!(batch.wait(TIMEOUT)?.transferred_bytes, 8);
        batch.free()?;
        let mut expected = vec![0x9a; f.memory.len()];
        if op == TransferOp::Read {
            expected[1..9].copy_from_slice(b"abcdefgh");
        }
        let mut bytes = vec![0; f.memory.len()];
        f.memory.read(0, &mut bytes)?;
        assert_eq!(bytes, expected);
        assert_eq!(
            fs::read(&f.path)?,
            if op == TransferOp::Write {
                vec![0x9a; 8]
            } else {
                b"abcdefgh".to_vec()
            }
        );
    }
    Ok(())
}

#[cfg(feature = "metrics")]
#[test]
fn inline_file_metrics_pair_each_public_request_with_one_terminal_attempt() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    for ready in [true, false] {
        let (f, _) = observed(ready, false, false)?;
        let mut batch = f.engine.submit_async(
            TransferOp::Read,
            &f.segment,
            &[f.request(1, 0, 8)],
            None,
            TIMEOUT,
        )?;
        assert_eq!(batch.wait(TIMEOUT)?.completed_tasks, 1);
        batch.free()?;
        let output = f.engine.metrics().unwrap().prometheus();
        for expected in [
            "orbite_read_requests_total{transport=\"io_uring\"} 1",
            "orbite_transport_attempts_total{transport=\"io_uring\",operation=\"read\"} 1",
            "orbite_transport_attempt_outcomes_total{transport=\"io_uring\",operation=\"read\",outcome=\"success\"} 1",
            "orbite_inflight_attempts{transport=\"io_uring\"} 0",
            "orbite_logical_requests_started_total{operation=\"read\"} 1",
            "orbite_logical_requests_inflight{operation=\"read\"} 0",
        ] {
            assert!(
                output.lines().any(|line| line == expected),
                "missing {expected}"
            );
        }
        assert!(output.contains("orbite_read_bytes_total{transport=\"io_uring\"} 8"));
    }
    Ok(())
}
