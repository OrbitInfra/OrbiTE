use super::*;
use crate::platform::numa::{NATIVE, Numa};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn failed_idle_unmap_keeps_reserved_bytes_after_eviction_and_last_drop() -> Result<()> {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static FAIL: Numa = Numa {
        unmap: |_, _| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Err(Error::Io(std::io::Error::from_raw_os_error(libc::EIO)))
        },
        ..NATIVE
    };
    let bytes = super::super::task::READ_PREFIX_BYTES;
    for stage in ["close", "admission", "drop"] {
        let pool = Pool::new(IoUringConfig::default(), bytes)?;
        let state = Arc::downgrade(&pool.state);
        let mut active = pool.charge_read(bytes, Some(4096))?;
        active.prefix = Some(Bounce::mapped(bytes, 0, &FAIL)?);
        active.finish()?;
        drop(active);
        let before = CALLS.load(Ordering::SeqCst);
        assert_eq!(pool.stats().idle_read_bytes, bytes);
        if stage == "admission" {
            assert!(matches!(
                pool.charge(1),
                Err(Error::Unquiesced {
                    code: libc::EIO,
                    ..
                })
            ));
        }
        if stage != "drop" {
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
                let stats = pool.stats();
                assert_eq!((stats.pageable_bytes, stats.idle_read_bytes), (0, bytes));
            }
        }
        drop(pool);
        assert_eq!(CALLS.load(Ordering::SeqCst), before + 1);
        let retained = state
            .upgrade()
            .expect("the failed mapping and its charge survive final drop");
        let retained = retained.lock().unwrap();
        assert!(retained.fault.is_some() && retained.read_prefix.is_some());
        assert_eq!(retained.stats.idle_read_bytes, bytes);
    }
    Ok(())
}
