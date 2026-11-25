use super::*;

pub(super) fn directory(variable: &str) -> Option<PathBuf> {
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

const PAGE: usize = 4096;

fn direct_fixture(
    dir: &Path,
    image: &[u8],
    ready: bool,
) -> Result<(Fixture, Arc<Mutex<Trace>>, usize)> {
    let (mut f, trace) = observed_at(dir, image, ready, false, false)?;
    f.memory = f.engine.register_memory(
        vec![0x9a; image.len() + 2 * PAGE].into_boxed_slice(),
        Permission::LocalOnly,
    )?;
    let base = f.memory.base_address();
    let start = (base.next_multiple_of(PAGE as u64) - base) as usize + PAGE;
    Ok((f, trace, start))
}

#[test]
fn large_direct_read_with_negative_cqe_preserves_the_entire_destination() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    const SIZE: usize = 64 << 20;
    let (f, trace, start) = direct_fixture(&dir, &vec![0xe7; SIZE], true)?;
    // The registered destination includes both guards; every old byte differs from the file.
    let expected: Vec<_> = (0..64 * 1024)
        .map(|i: usize| ((i ^ (i >> 8)) & 0x7f) as u8)
        .collect();
    for at in (0..f.memory.len()).step_by(expected.len()) {
        f.memory
            .write(at, &expected[..expected.len().min(f.memory.len() - at)])?;
    }
    // One 64 MiB SQE yields -EIO. Smaller native reads can finish 8 MiB
    // before the injected failure in this same public task.
    trace.lock().unwrap().read_bytes_before_eio = Some(8 << 20);
    let mut batch = f.engine.submit_async(
        TransferOp::Read,
        &f.segment,
        &[TransferRequest::new(
            Transport::IoUring,
            f.memory.base_address() + start as u64,
            0,
            SIZE,
        )],
        None,
        TIMEOUT,
    )?;
    let status = batch.wait(TIMEOUT)?;
    let task = batch.task_status(0)?;
    assert!(trace.lock().unwrap().read_bytes_before_eio.is_none());
    assert_eq!(
        f.engine.file_segment_info(&f.segment)?.direct_io,
        Some(true)
    );
    assert!(status.locally_drained && task.locally_drained);
    assert_eq!((task.state, task.transferred_bytes), (TaskState::Failed, 0));
    assert!(
        matches!(&task.failure.unwrap().error, Error::Backend { code, .. } if *code == -libc::EIO)
    );
    let mut actual = vec![0; expected.len()];
    for at in (0..f.memory.len()).step_by(expected.len()) {
        let len = actual.len().min(f.memory.len() - at);
        f.memory.read(at, &mut actual[..len])?;
        assert!(
            actual[..len] == expected[..len],
            "destination changed in chunk at {at}"
        );
    }
    batch.free()?;
    let stats = f.engine.stats()?;
    let file = stats.io_uring.unwrap();
    assert_eq!(
        (stats.inflight_bytes, file.pageable_bytes, file.active_slots),
        (0, 0, 0)
    );
    Ok(())
}

#[test]
#[allow(unsafe_code)]
fn idle_direct_read_ring_yields_its_fd_to_larger_and_scalar_submissions() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    const CHILD: &str = "ORBITE_IDLE_RING_FD_LIMIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let run = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "transport::io_uring::tests::inline::group::idle_direct_read_ring_yields_its_fd_to_larger_and_scalar_submissions"])
            .env(CHILD, "1")
            .output()?;
        let out = String::from_utf8_lossy(&run.stdout);
        let err = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success() && out.contains("1 passed"),
            "{out}\n{err}"
        );
        return Ok(());
    }
    let limit = libc::rlimit {
        rlim_cur: 64,
        rlim_max: 64,
    };
    // SAFETY: limit is initialized and this child runs only the fd pressure test.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let image: Vec<_> = (0..8 * PAGE).map(|i| (i % 251) as u8).collect();
    for width in [8, 1] {
        let (f, trace, start) = direct_fixture(&dir, &image, true)?;
        warm(&f, &trace)?;
        trace.lock().unwrap().allow_reuse = true;
        let reqs: Vec<_> = (0..8)
            .map(|i| f.request(start + i * PAGE, (i * PAGE) as u64, PAGE))
            .collect();
        let mut b =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &reqs[..2], None, TIMEOUT)?;
        assert_eq!(b.wait(TIMEOUT)?.completed_tasks, 2);
        b.free()?;
        assert!(trace.lock().unwrap().alive.upgrade().is_some());
        let mut expected = vec![0x9a; f.memory.len()];
        f.memory.write(0, &expected)?;
        let mut held = Vec::new();
        loop {
            match fs::File::open("/dev/null") {
                Ok(fd) => held.push(fd),
                Err(e) if e.raw_os_error() == Some(libc::EMFILE) => break,
                Err(e) => return Err(e.into()),
            }
        }
        let mut b =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &reqs[..width], None, TIMEOUT)?;
        let st = b.wait(TIMEOUT)?;
        assert_eq!(
            (st.completed_tasks, st.transferred_bytes),
            (width, width * PAGE)
        );
        b.free()?;
        drop(held);
        // Growing evicts before setup. An unrelated scalar setup retries once.
        let t = trace.lock().unwrap();
        assert_eq!(t.setups, if width == 8 { 2 } else { 3 });
        assert_eq!(t.submits, 2);
        drop(t);
        expected[start..start + width * PAGE].copy_from_slice(&image[..width * PAGE]);
        let mut actual = vec![0; expected.len()];
        f.memory.read(0, &mut actual)?;
        assert_eq!(actual, expected);
    }
    Ok(())
}

#[test]
fn drained_direct_read_ring_is_bounded_and_short_cqes_retire_it() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image = vec![0x76; 8 * PAGE];
    let (f, trace, start) = direct_fixture(&dir, &image, true)?;
    warm(&f, &trace)?;
    trace.lock().unwrap().allow_reuse = true;
    assert_eq!(
        f.engine.file_segment_info(&f.segment)?.direct_io,
        Some(true)
    );
    let reqs: Vec<_> = (0..8)
        .map(|i| f.request(start + i * PAGE, (i * PAGE) as u64, PAGE))
        .collect();
    for _ in 0..2 {
        let mut b = f
            .engine
            .submit_async(TransferOp::Read, &f.segment, &reqs, None, TIMEOUT)?;
        assert_eq!(b.wait(TIMEOUT)?.completed_tasks, 8);
        assert!(
            b.task_statuses()?
                .iter()
                .all(|t| t.transferred_bytes == PAGE && t.locally_drained)
        );
        b.free()?;
        assert_eq!(trace.lock().unwrap().setups, 1);
    }
    let mut bytes = vec![0; f.memory.len()];
    f.memory.read(0, &mut bytes)?;
    assert_eq!(&bytes[start..start + image.len()], image.as_slice());
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    f.memory.write(0, &bytes)?;
    // Shrink only after range admission, before the cached ring publishes SQEs.
    trace.lock().unwrap().truncate_before_push =
        Some((f.path.clone(), (image.len() - PAGE + 512) as u64));
    let mut b = f
        .engine
        .submit_async(TransferOp::Read, &f.segment, &reqs, None, TIMEOUT)?;
    let st = b.wait(TIMEOUT)?;
    assert!(st.locally_drained);
    assert_eq!(
        (st.completed_tasks, st.transferred_bytes),
        (7, 7 * PAGE + 512)
    );
    let prefix = 7 * PAGE + 512;
    bytes[start..start + prefix].copy_from_slice(&image[..prefix]);
    let mut actual = vec![0; bytes.len()];
    f.memory.read(0, &mut actual)?;
    assert_eq!(actual, bytes);
    b.free()?;
    assert!(
        f.engine
            .io_uring
            .as_ref()
            .unwrap()
            .idle_read
            .lock()
            .unwrap()
            .take(8)
            .is_none()
    );
    fs::write(&f.path, &image)?;
    let mut b = f
        .engine
        .submit_async(TransferOp::Read, &f.segment, &reqs, None, TIMEOUT)?;
    assert_eq!(b.wait(TIMEOUT)?.completed_tasks, 8);
    b.free()?;
    assert_eq!(trace.lock().unwrap().setups, 2);
    f.engine.close(TIMEOUT)?;
    assert!(
        f.engine
            .io_uring
            .as_ref()
            .unwrap()
            .idle_read
            .lock()
            .unwrap()
            .take(8)
            .is_none()
    );
    Ok(())
}

#[test]
fn direct_read_group_keeps_eight_receipts_across_partial_submit_and_handoff() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image: Vec<u8> = (0..8 * PAGE).map(|i| (i % 251) as u8).collect();
    for (ready, limit) in [
        (true, None),
        (false, None),
        (true, Some(3)),
        (false, Some(3)),
    ] {
        let (f, trace, start) = direct_fixture(&dir, &image, ready)?;
        warm(&f, &trace)?;
        assert_eq!(
            f.engine.file_segment_info(&f.segment)?.direct_io,
            Some(true)
        );
        let mut expected = vec![0; f.memory.len()];
        f.memory.read(0, &mut expected)?;
        trace.lock().unwrap().submit_limit = limit;
        let requests = (0..8)
            .map(|i| f.request(start + i * PAGE, (i * PAGE) as u64, PAGE))
            .collect::<Vec<_>>();
        let workers = WORKER_STARTS.with(|n| n.get());
        let mut batch =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &requests, None, TIMEOUT)?;
        let status = batch.wait(TIMEOUT)?;
        let count = limit.unwrap_or(8) as usize;
        assert!(status.locally_drained);
        assert_eq!(
            (status.completed_tasks, status.transferred_bytes),
            (count, count * PAGE)
        );
        for (i, task) in batch.task_statuses()?.iter().enumerate() {
            assert!(task.locally_drained);
            assert_eq!(task.physical_owner, i);
            if i < count {
                assert_eq!(
                    (task.state, task.transferred_bytes),
                    (TaskState::Completed, PAGE)
                );
            } else {
                assert_eq!(task.transferred_bytes, 0);
                assert_eq!(
                    task.failure.as_ref().unwrap().remote_effect,
                    RemoteEffect::None
                );
            }
        }
        expected[start..start + count * PAGE].copy_from_slice(&image[..count * PAGE]);
        let mut bytes = vec![0; expected.len()];
        f.memory.read(0, &mut bytes)?;
        assert_eq!(bytes, expected);
        assert_eq!(fs::read(&f.path)?, image);
        assert_eq!(
            WORKER_STARTS.with(|n| n.get()) - workers,
            usize::from(!ready)
        );
        let t = trace.lock().unwrap();
        assert_eq!(t.submitter, Some(std::thread::current().id()));
        assert_eq!(t.submits, if limit.is_some() { 2 } else { 1 });
        assert_eq!(t.group_checks.len(), 1);
        assert_eq!(t.group_checks[0].0, count as u32);
        assert!(t.alive.upgrade().is_none());
        drop(t);
        batch.free()?;
        assert_eq!(f.engine.stats()?.inflight_bytes, 0);
    }
    Ok(())
}

#[test]
fn direct_groups_that_need_a_worker_keep_worker_submission() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image = vec![0x41; 4 * PAGE];
    let read = TransferOp::Read;
    // Vary just one eligibility condition from a warmed, aligned, adjacent READ.
    for (op, cold, local_offset, file_offset, length, gap) in [
        (TransferOp::Write, false, 0, 0, PAGE, 0),
        (read, true, 0, 0, PAGE, 0),
        (read, false, 0, 0, PAGE, PAGE),
        (read, false, 1, 0, PAGE, 0),
        (read, false, 0, 1, PAGE, 0),
        (read, false, 0, 0, PAGE - 1, 0),
    ] {
        let (f, trace, start) = direct_fixture(&dir, &image, true)?;
        if !cold {
            warm(&f, &trace)?;
        }
        let requests = [
            f.request(start, file_offset, length),
            f.request(
                start + PAGE + local_offset,
                file_offset + (length + gap) as u64,
                length,
            ),
        ];
        let workers = WORKER_STARTS.with(|n| n.get());
        let mut batch = f
            .engine
            .submit_async(op, &f.segment, &requests, None, TIMEOUT)?;
        assert_eq!(batch.wait(TIMEOUT)?.completed_tasks, 2);
        assert_eq!(WORKER_STARTS.with(|n| n.get()) - workers, 1);
        let t = trace.lock().unwrap();
        assert_ne!(t.submitter, Some(std::thread::current().id()));
        assert!(t.group_checks.is_empty());
        drop(t);
        batch.free()?;
    }
    Ok(())
}

#[test]
fn direct_read_copies_only_each_cqes_prefix_after_truncate() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image: Vec<_> = (0..4 * PAGE).map(|i| (i % 251) as u8).collect();
    for ready in [true, false] {
        for tail in [0, 1, 512, PAGE - 1] {
            let (f, trace, start) = direct_fixture(&dir, &image, ready)?;
            warm(&f, &trace)?;
            let mut expected = vec![0; f.memory.len()];
            f.memory.read(0, &mut expected)?;
            // Truncate after validation and before the kernel consumes any SQE.
            trace.lock().unwrap().truncate_to = Some((2 * PAGE + tail) as u64);
            let requests = (0..3)
                .map(|i| f.request(start + i * PAGE, ((i + 1) * PAGE) as u64, PAGE))
                .collect::<Vec<_>>();
            let mut batch =
                f.engine
                    .submit_async(TransferOp::Read, &f.segment, &requests, None, TIMEOUT)?;
            let status = batch.wait(TIMEOUT)?;
            assert!(status.locally_drained);
            assert_eq!(
                (status.completed_tasks, status.transferred_bytes),
                (1, PAGE + tail)
            );
            for (task, (state, n)) in batch.task_statuses()?.iter().zip([
                (TaskState::Completed, PAGE),
                (TaskState::Failed, tail),
                (TaskState::Failed, 0),
            ]) {
                assert_eq!(
                    (task.state, task.transferred_bytes, task.locally_drained),
                    (state, n, true)
                );
            }
            expected[start..start + PAGE + tail].copy_from_slice(&image[PAGE..2 * PAGE + tail]);
            let mut actual = vec![0; expected.len()];
            f.memory.read(0, &mut actual)?;
            assert_eq!(actual, expected);
            batch.free()?;
            assert_eq!(f.engine.stats()?.io_uring.unwrap().pageable_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn large_direct_read_uses_device_alignment_when_huge_alignment_is_unavailable() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static HINT_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let length = 2 << 20;
    let image: Vec<_> = (0..length).map(|i| (i % 251) as u8).collect();
    for prefix in [length, length - PAGE + 511] {
        let (mut f, trace, start) = direct_fixture(&dir, &image, true)?;
        Arc::get_mut(&mut f.engine)
            .unwrap()
            .io_uring
            .as_mut()
            .unwrap()
            .allocate = |len, align| {
            if align > PAGE {
                HINT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                Err(Error::ResourceExhausted)
            } else {
                task::Bounce::new(len, align)
            }
        };
        warm(&f, &trace)?;
        assert_eq!(
            f.engine.file_segment_info(&f.segment)?.direct_io,
            Some(true)
        );
        let mut expected = vec![0; f.memory.len()];
        f.memory.read(0, &mut expected)?;
        expected[start..start + prefix].copy_from_slice(&image[..prefix]);
        if prefix < length {
            trace.lock().unwrap().truncate_to = Some(prefix as u64);
        }
        let before = HINT_ATTEMPTS.load(Ordering::Relaxed);
        let mut batch = f.engine.submit_async(
            TransferOp::Read,
            &f.segment,
            &[f.request(start, 0, length)],
            None,
            TIMEOUT,
        )?;
        let status = batch.wait(TIMEOUT)?;
        assert_eq!(HINT_ATTEMPTS.load(Ordering::Relaxed) - before, 1);
        assert!(status.locally_drained);
        assert_eq!(status.transferred_bytes, prefix);
        assert_eq!(status.completed_tasks, usize::from(prefix == length));
        let mut received = vec![0; expected.len()];
        f.memory.read(0, &mut received)?;
        assert_eq!(received, expected);
        batch.free()?;
        assert_eq!(f.engine.stats()?.io_uring.unwrap().pageable_bytes, 0);
    }
    Ok(())
}

#[test]
fn direct_vector_read_preserves_ranges_beyond_eof_and_skips_padding() -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image: Vec<_> = (0..6 * PAGE).map(|i| (i % 251) as u8).collect();
    for padding in [false, true] {
        let (f, trace, start) = direct_fixture(&dir, &image, true)?;
        let address = f.memory.base_address() + start as u64;
        let mut parts = vec![(address + (2 * PAGE) as u64, PAGE).into()];
        if padding {
            parts.push(crate::FileIoElement::Padding(PAGE));
        }
        parts.push((address, PAGE).into());
        parts.push((address + PAGE as u64, PAGE).into());
        let req = TransferRequest::file_vectored(Transport::IoUring, PAGE as u64, &parts)?;
        let second_offset = (2 + usize::from(padding)) * PAGE;
        trace.lock().unwrap().truncate_to = Some((second_offset + 512) as u64);
        let mut batch =
            f.engine
                .submit_async(TransferOp::Read, &f.segment, &[req], None, TIMEOUT)?;
        let status = batch.wait(TIMEOUT)?;
        assert!(status.locally_drained);
        assert_eq!(status.transferred_bytes, second_offset + 512 - PAGE);
        assert_eq!(batch.task_status(0)?.state, TaskState::Failed);
        let mut expected = vec![0x9a; f.memory.len()];
        expected[start + 2 * PAGE..start + 3 * PAGE].copy_from_slice(&image[PAGE..2 * PAGE]);
        expected[start..start + 512].copy_from_slice(&image[second_offset..second_offset + 512]);
        let mut actual = vec![0; expected.len()];
        f.memory.read(0, &mut actual)?;
        assert_eq!(actual, expected);
        batch.free()?;
        assert_eq!(f.engine.stats()?.io_uring.unwrap().pageable_bytes, 0);
    }
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
