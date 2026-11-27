use super::*;
use crate::platform::numa::{NATIVE, Numa};
use std::sync::atomic::{AtomicUsize, Ordering};

const CASE: &str = "ORBITE_TEST_DIRECT_READ_ROLLBACK";
const SIZE: usize = 64 << 20;
static UNMAPS: AtomicUsize = AtomicUsize::new(0);
static FAIL: Numa = Numa {
    unmap: |_, _| {
        UNMAPS.fetch_add(1, Ordering::SeqCst);
        Err(Error::Io(io::Error::from_raw_os_error(libc::EIO)))
    },
    ..NATIVE
};

#[test]
fn direct_read_preflight_and_unsubmitted_suffix_report_retirement() -> Result<()> {
    if let Some(case) = std::env::var_os(CASE) {
        return run(case.to_str().unwrap());
    }
    // Failed unmaps deliberately retain the native allocation and its admission.
    // Reap each case before starting the next one, including the clean controls.
    for path in ["scalar-setup", "general-setup", "group-push", "group-tail"] {
        for cleanup in ["clean", "fail"] {
            let case = format!("{path}/{cleanup}");
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "transport::io_uring::tests::inline::group::rollback::direct_read_preflight_and_unsubmitted_suffix_report_retirement", "--nocapture"])
                .env(CASE, &case)
                .output()?;
            let result = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && result.contains("1 passed; 0 failed;"),
                "{case}: {result} {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(())
}

fn run(case: &str) -> Result<()> {
    let Some(dir) = directory("ORBITE_EXT4_DIR") else {
        return Ok(());
    };
    if !kernel() {
        return Ok(());
    }
    let (path, cleanup) = case.split_once('/').unwrap();
    let group = path != "scalar-setup";
    let submitted = usize::from(path == "group-tail");
    let (mut f, trace, start) = direct_fixture(&dir, &vec![0x39; SIZE + 2 * PAGE], true)?;
    {
        let io = Arc::get_mut(&mut f.engine)
            .unwrap()
            .io_uring
            .as_mut()
            .unwrap();
        if cleanup == "fail" {
            io.mappings = &FAIL;
        }
        match path {
            "scalar-setup" | "general-setup" => {
                let trace = trace.clone();
                io.create = Arc::new(move |_| {
                    trace.lock().unwrap().setups += 1;
                    Err(io::Error::from_raw_os_error(libc::ENOSPC))
                });
            }
            "group-push" => trace.lock().unwrap().reject_push = Some((1, libc::ENOSPC)),
            "group-tail" => trace.lock().unwrap().submit_limit = Some(1),
            _ => panic!("unknown rollback path {path}"),
        }
    }
    let mut requests = Vec::new();
    if group {
        requests.push(f.request(start - PAGE, 0, PAGE));
    }
    // A file offset gap prevents these two independent requests from merging.
    requests.push(f.request(start, if group { (2 * PAGE) as u64 } else { 0 }, SIZE));
    let mut batch =
        f.engine
            .submit_async(TransferOp::Read, &f.segment, &requests, None, TIMEOUT)?;
    let status = batch.wait(TIMEOUT)?;
    let last = batch.task_status(requests.len() - 1)?;
    let failed = cleanup == "fail";
    assert_eq!(status.locally_drained, !failed, "batch drain in {case}");
    assert_eq!(
        last.locally_drained, !failed,
        "failed owner's drain in {case}"
    );
    assert_eq!(
        (status.completed_tasks, status.transferred_bytes),
        (submitted, submitted * PAGE)
    );
    assert_eq!(last.transferred_bytes, 0);
    let blamed = if !failed && submitted == 0 {
        batch.task_status(0)?.failure
    } else {
        last.failure
    };
    let error = &blamed.expect("failure must identify its owner").error;
    {
        let t = trace.lock().unwrap();
        assert_eq!(t.setups, 1);
        assert_eq!(
            t.pushes,
            if path.starts_with("group-") {
                vec![2]
            } else {
                Vec::new()
            }
        );
        assert_eq!(t.reaps, submitted, "every kernel-consumed SQE must drain");
        if submitted == 0 {
            assert_eq!(t.submits, 0);
        }
    }
    if group {
        let first = batch.task_status(0)?;
        assert!(
            first.locally_drained,
            "the small sibling has no uncertain mapping in {case}"
        );
        let mut head = [0u8; PAGE];
        f.memory.read(start - PAGE, &mut head)?;
        if submitted == 1 {
            assert_eq!(
                (first.state, first.transferred_bytes),
                (TaskState::Completed, PAGE)
            );
            assert_eq!(head, [0x39; PAGE]);
        } else {
            assert_eq!(first.transferred_bytes, 0);
            assert!(
                !first
                    .failure
                    .as_ref()
                    .is_some_and(|f| matches!(f.error, Error::Unquiesced { .. }))
            );
            assert_eq!(head, [0x9a; PAGE]);
        }
    }
    let stats = f.engine.stats()?;
    let file = stats.io_uring.unwrap();
    if failed {
        assert!(
            matches!(
                error,
                Error::Unquiesced {
                    code: libc::EIO,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(matches!(batch.free(), Err(Error::BatchBusy)));
        assert_eq!(file.pageable_bytes, SIZE);
        assert_eq!(file.idle_read_bytes, 0);
        assert_eq!(stats.inflight_bytes, SIZE + usize::from(group) * PAGE);
        assert!(f.memory.read(start, &mut [0; 1]).is_err());
        assert!(f.memory.write(start, &[0x22]).is_err());
        let pool = &f.engine.io_uring.as_ref().unwrap().pool;
        for _ in 0..2 {
            assert!(matches!(
                pool.close(),
                Err(Error::Unquiesced {
                    code: libc::EIO,
                    ..
                })
            ));
            assert!(matches!(
                pool.charge(1),
                Err(Error::Unquiesced {
                    code: libc::EIO,
                    ..
                })
            ));
            assert_eq!(UNMAPS.load(Ordering::SeqCst), 1, "failed cleanup retried");
        }
        drop(batch);
        assert!(matches!(
            f.engine.close_file_segment(&f.segment, Duration::ZERO),
            Err(Error::Timeout)
        ));
    } else {
        let errno = if submitted == 0 {
            libc::ENOSPC
        } else {
            libc::EIO
        };
        assert!(
            match error {
                Error::Io(e) => e.raw_os_error() == Some(errno),
                Error::Backend { code, .. } => *code == -errno,
                _ => false,
            },
            "lost original error {error:?}"
        );
        batch.free()?;
        assert_eq!((stats.inflight_bytes, file.pageable_bytes), (0, 0));
        let mut sentinel = [0; PAGE];
        f.memory.read(start, &mut sentinel)?;
        assert_eq!(sentinel, [0x9a; PAGE]);
        f.engine.close_file_segment(&f.segment, TIMEOUT)?;
    }
    Ok(())
}
