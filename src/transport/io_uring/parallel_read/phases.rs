//! Paired diagnostic for the scalar owner. Measurements use one worker
//! thread for the complete submit/drain sequence, and process-wide CPU for all threads.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
struct Caller {
    pid: u32,
    tid: libc::pid_t,
    clock: libc::clockid_t,
}

fn caller() -> Option<Caller> {
    static CALLER: OnceLock<Option<Caller>> = OnceLock::new();
    *CALLER.get_or_init(|| {
        let receipt = std::env::var("ORBITE_DIAG_CALLER_CLOCK_RECEIPT").ok()?;
        let mut values = receipt.split(',');
        let pid = values.next()?.parse().ok()?;
        let tid: libc::pid_t = values.next()?.parse().ok()?;
        let clock = values.next()?.parse().ok()?;
        (values.next().is_none() && pid == std::process::id() && tid as u32 == pid && clock < 0)
            .then_some(Caller { pid, tid, clock })
    })
}

pub(in crate::transport::io_uring) fn split_enabled() -> bool {
    static TWO: OnceLock<bool> = OnceLock::new();
    *TWO.get_or_init(|| std::env::var("ORBITE_DIAG_SPLIT_MODE").as_deref() != Ok("one"))
}

#[derive(Clone, Copy)]
struct Stamp {
    cpu: u64,
    thread: u64,
    caller: Option<u64>,
    wall: u64,
}

impl Stamp {
    fn now(caller: Option<Caller>) -> Option<Self> {
        #[allow(unsafe_code)]
        fn read(clock: libc::clockid_t) -> Option<u64> {
            let mut value = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: clock_gettime initializes a live writable timespec.
            (unsafe { libc::clock_gettime(clock, &mut value) } == 0)
                .then(|| value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64)
        }
        Some(Self {
            cpu: read(libc::CLOCK_PROCESS_CPUTIME_ID)?,
            thread: read(libc::CLOCK_THREAD_CPUTIME_ID)?,
            caller: match caller {
                Some(c) => Some(read(c.clock)?),
                None => None,
            },
            wall: read(libc::CLOCK_MONOTONIC)?,
        })
    }
}

pub(super) struct PhaseClock {
    bounds: Option<[Option<Stamp>; 7]>,
    caller: Option<(Caller, usize, libc::pid_t)>,
}

impl PhaseClock {
    pub(super) fn start() -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled =
            *ENABLED.get_or_init(|| std::env::var_os("ORBITE_DIAG_SPLIT_PHASES").is_some());
        let caller = enabled.then(caller).flatten().map(|c| {
            static INDEX: AtomicUsize = AtomicUsize::new(0);
            #[allow(unsafe_code)]
            // SAFETY: gettid has no parameters and reports the thread executing this entire owner.
            let owner_tid = unsafe { libc::gettid() };
            (c, INDEX.fetch_add(1, Ordering::Relaxed), owner_tid)
        });
        let mut this = Self {
            bounds: enabled.then_some([None; 7]),
            caller,
        };
        this.mark(0);
        this
    }

    pub(super) fn mark(&mut self, index: usize) {
        if let Some(bounds) = &mut self.bounds {
            bounds[index] = Stamp::now(self.caller.map(|(c, _, _)| c));
        }
    }

    pub(super) fn skip_second(&mut self) {
        if let Some(bounds) = &mut self.bounds {
            bounds[3] = bounds[2];
        }
    }

    pub(super) fn emit(&self, pieces: usize) {
        let Some(bounds) = &self.bounds else {
            return;
        };
        let names = [
            "setup",
            "first_enter",
            "second",
            "drain",
            "publish",
            "retire",
        ];
        let mut fields = vec![format!(r#""pieces":{pieces}"#)];
        if let Some((c, index, owner_tid)) = self.caller {
            fields.push(format!(r#""index":{index},"pid":{},"owner_tid":{owner_tid},"caller_tid":{},"pthread_clock":{}"#,c.pid,c.tid,c.clock));
        }
        for (name, pair) in names.iter().zip(bounds.windows(2)) {
            let (Some(a), Some(b)) = (pair[0], pair[1]) else {
                return;
            };
            if let (Some(x), Some(y)) = (a.caller, b.caller) {
                fields.push(format!(r#""{name}_caller_ns":{}"#, y - x));
            }
            fields.push(format!(
                r#""{name}_cpu_ns":{},"{name}_thread_ns":{},"{name}_wall_ns":{}"#,
                b.cpu - a.cpu,
                b.thread - a.thread,
                b.wall - a.wall
            ));
        }
        eprintln!("SPLIT_PHASE {{{}}}", fields.join(","));
    }
}
