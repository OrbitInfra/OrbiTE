//! Private benchmark instrumentation included only in this experimental branch.
use std::{ptr::NonNull, sync::OnceLock, time::Instant};

#[derive(Clone, Copy)]
pub(super) struct Stamp(Instant, u64, i64);
impl Stamp {
    #[allow(unsafe_code)]
    pub(super) fn now() -> Self {
        let mut cpu = std::mem::MaybeUninit::uninit();
        let mut usage = std::mem::MaybeUninit::uninit();
        // SAFETY: the kernel initializes the timestamp and usage records on success.
        unsafe {
            assert_eq!(
                libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, cpu.as_mut_ptr()),
                0
            );
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
            let ts = cpu.assume_init();
            Self(
                Instant::now(),
                ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64,
                usage.assume_init().ru_minflt as i64,
            )
        }
    }
    fn since(self, start: Self) -> [u64; 3] {
        [
            self.0.duration_since(start.0).as_nanos() as u64,
            self.1 - start.1,
            (self.2 - start.2) as u64,
        ]
    }
}
pub(super) struct Probe {
    start: Stamp,
    allocated: Stamp,
    touched: Stamp,
    cqe: Stamp,
    copied: Stamp,
    prefault: bool,
}
impl Probe {
    pub(super) fn start(len: usize, direct: bool) -> Option<Box<Self>> {
        static MODE: OnceLock<Option<bool>> = OnceLock::new();
        if !direct || len != 64 << 20 {
            return None;
        }
        let prefault =
            (*MODE.get_or_init(|| match std::env::var("ORBITE_LARGE_PHASE").as_deref() {
                Ok("trace") => Some(false),
                Ok("prefault") => Some(true),
                _ => None,
            }))?;
        let now = Stamp::now();
        Some(Box::new(Self {
            start: now,
            allocated: now,
            touched: now,
            cqe: now,
            copied: now,
            prefault,
        }))
    }
    #[allow(unsafe_code)]
    pub(super) fn ready(&mut self, ptr: NonNull<u8>, len: usize) {
        self.allocated = Stamp::now();
        if self.prefault {
            for off in (0..len).step_by(4096) {
                // SAFETY: caller exclusively owns the whole private writable scratch before building SQEs.
                unsafe {
                    ptr.as_ptr().add(off).write_volatile(17);
                }
            }
        }
        self.touched = Stamp::now();
    }
    pub(super) fn cqe(&mut self) {
        self.cqe = Stamp::now();
    }
    pub(super) fn copied(&mut self) {
        self.copied = Stamp::now();
    }
    pub(super) fn emit(self, result: i32) {
        let end = Stamp::now();
        eprintln!(
            "owner-phases {result} {:?} {:?} {:?} {:?} {:?}",
            self.allocated.since(self.start),
            self.touched.since(self.allocated),
            self.cqe.since(self.touched),
            self.copied.since(self.cqe),
            end.since(self.copied)
        );
    }
}
