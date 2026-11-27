use super::*;

const SIZE: usize = 64 << 20;
const PAGE: usize = 4096;

#[derive(Clone, Copy)]
enum Receipt {
    Full,
    Truncate(usize),
    LateError,
}

#[test]
fn external_file_vectors_publish_confirmed_bytes_and_retain_loans_until_drain() -> Result<()> {
    let Some(dir) = group::directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let image: Vec<_> = (0..SIZE)
        .map(|i| 0x80 | (((i >> 17) ^ (i >> 12) ^ i) & 0x7f) as u8)
        .collect();
    // The third layout adds a synthetic, sector-aligned verification range so
    // late direct errors also exercise a final, independently owned destination.
    let sector = 512;
    for lengths in [
        &[SIZE][..],
        &[SIZE / 2, SIZE / 2],
        &[SIZE / 2, SIZE / 2 - sector, sector],
    ] {
        for receipt in [
            Receipt::Full,
            Receipt::Truncate(SIZE / 2 + 13),
            Receipt::LateError,
        ] {
            check_extent(&dir, &image, lengths, receipt)?;
        }
    }
    // OrbitKV reads a sub-sector padding tail into its own registered range for
    // verification. The final 128 bytes are visible, including an EOF inside them.
    for receipt in [Receipt::Full, Receipt::Truncate(SIZE - 7)] {
        check_extent(&dir, &image, &[SIZE / 2, SIZE / 2 - 128, 128], receipt)?;
    }
    Ok(())
}

fn check_extent(dir: &Path, image: &[u8], lengths: &[usize], receipt: Receipt) -> Result<()> {
    let (f, trace) = observed_at(dir, image, true, false, false)?;
    let mut buffers = Vec::new();
    let mut ranges = Vec::new();
    for (index, &len) in lengths.iter().enumerate() {
        let bytes: Vec<_> = (0..len + 2 * PAGE).map(|i| original(index, i)).collect();
        let external = crate::test_support::external_memory(&bytes);
        let base = external.memory.base_address();
        // Both guards must be nonempty, even when the base was page aligned.
        let start = ((base + 1).next_multiple_of(PAGE as u64) - base) as usize;
        ranges.push((external.memory.clone(), start, len));
        buffers.push(external);
    }
    let handles = f
        .engine
        .register_host_memory_batch(&ranges, Permission::LocalOnly)?;
    let elements: Vec<_> = handles
        .iter()
        .map(|h| (h.base_address(), h.len()))
        .collect();
    let request = TransferRequest::file_vectored(Transport::IoUring, 0, &elements)?;
    // A 4Kn device can require buffered I/O for the fractional-page tail.
    // Confirm which descriptor the production selector chooses for these exact ranges.
    let direct = {
        let (lease, _) = f
            .engine
            .files
            .admit(&f.segment, TransferOp::Read, 0, SIZE)?;
        let iovecs: Vec<_> = elements
            .iter()
            .map(|&(address, length)| libc::iovec {
                iov_base: address as usize as *mut _,
                iov_len: length,
            })
            .collect();
        let (file, _) = lease.file_for_iovecs(0, &iovecs)?;
        lease.is_direct_read(TransferOp::Read, &file)
    };
    if lengths.last() == Some(&128) {
        assert!(!direct, "the sub-sector ext4 tail must select buffered I/O");
    }
    if !direct {
        assert!(
            lengths.len() == 3 && lengths[2] <= 512,
            "aligned ext4 vector must use direct I/O"
        );
        if matches!(receipt, Receipt::LateError) {
            eprintln!(
                "ext4 selected buffered I/O for the 512-byte range; direct late errors run on both page-aligned layouts"
            );
            return Ok(());
        }
    }
    let prefix = match receipt {
        Receipt::Full => SIZE,
        Receipt::Truncate(prefix) => {
            trace.lock().unwrap().truncate_before_push = Some((f.path.clone(), prefix as u64));
            prefix
        }
        Receipt::LateError => {
            trace.lock().unwrap().read_bytes_before_eio = Some(8 << 20);
            0
        }
    };
    let (observed, arrived) = mpsc::sync_channel(1);
    let (resume, released) = mpsc::sync_channel(1);
    trace.lock().unwrap().hold_read_receipt = Some((observed, released));
    let (status, task) = std::thread::scope(|scope| -> Result<_> {
        let submit = scope.spawn(|| -> Result<_> {
            let mut batch = f.engine.submit_async(
                TransferOp::Read,
                &f.segment,
                &[request],
                None,
                EVENTUAL_PROGRESS,
            )?;
            let status = batch.wait(EVENTUAL_PROGRESS)?;
            let task = batch.task_status(0)?;
            batch.free()?;
            Ok((status, task))
        });
        // The kernel already wrote a positive number of bytes. The transport
        // has not yet received the CQE that allows it to release the vector.
        assert!(arrived.recv_timeout(EVENTUAL_PROGRESS).unwrap() > 0);
        assert_eq!(f.engine.stats()?.inflight_bytes, SIZE);
        for handle in handles {
            f.engine.unregister_memory(&handle)?;
        }
        // Removing every registration closes admission while the already-posted
        // native request still owns every destination and its full byte charge.
        assert_eq!(f.engine.stats()?.inflight_bytes, SIZE);
        for (memory, start, _) in &ranges {
            assert!(matches!(
                memory.read(*start, &mut [0]),
                Err(Error::Unavailable(_))
            ));
        }
        resume.send(()).unwrap();
        submit.join().unwrap()
    })?;
    assert!(status.locally_drained && task.locally_drained);
    assert_eq!(
        (status.transferred_bytes, task.transferred_bytes),
        (prefix, prefix)
    );
    assert_eq!(status.completed_tasks, usize::from(prefix == SIZE));
    match receipt {
        Receipt::Full => assert_eq!(task.state, TaskState::Completed),
        Receipt::Truncate(_) => {
            assert_eq!(task.state, TaskState::Failed);
            assert!(
                matches!(&task.failure.unwrap().error, Error::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof)
            );
            assert!(trace.lock().unwrap().truncate_before_push.is_none());
        }
        Receipt::LateError => {
            assert_eq!(task.state, TaskState::Failed);
            assert!(
                matches!(task.failure.unwrap().error, Error::Backend { code, .. } if code == -libc::EIO)
            );
            assert!(trace.lock().unwrap().read_bytes_before_eio.is_none());
        }
    }
    let mut file_offset = 0;
    let mut chunk = vec![0; 64 << 10];
    for (index, (memory, start, len)) in ranges.into_iter().enumerate() {
        let published = prefix.saturating_sub(file_offset).min(len);
        for at in (0..memory.len()).step_by(chunk.len()) {
            let n = chunk.len().min(memory.len() - at);
            memory.read(at, &mut chunk[..n])?;
            assert!(
                chunk[..n].iter().enumerate().all(|(j, &byte)| {
                    let i = at + j;
                    byte == if (start..start + published).contains(&i) {
                        image[file_offset + i - start]
                    } else {
                        original(index, i)
                    }
                }),
                "corrupt destination {index}, chunk {at}, confirmed prefix {prefix}"
            );
        }
        file_offset += len;
    }
    assert_eq!(file_offset, SIZE);
    for buffer in buffers {
        drop(buffer.memory);
        assert!(buffer.owner.upgrade().is_none());
    }
    let stats = f.engine.stats()?;
    let file = stats.io_uring.unwrap();
    assert_eq!(
        (stats.inflight_bytes, file.pageable_bytes, file.active_slots),
        (0, 0, 0)
    );
    Ok(())
}

fn original(index: usize, offset: usize) -> u8 {
    ((index ^ offset ^ (offset >> 8)) & 0x7f) as u8
}
