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

pub(in crate::transport::io_uring) fn idle_mode() -> &'static str {
    static MODE: OnceLock<String> = OnceLock::new();
    MODE.get_or_init(|| {
        std::env::var("ORBITE_DIAG_SINGLE_IDLE").unwrap_or_else(|_| "reclaim".into())
    })
}

#[allow(unsafe_code)]
pub(super) fn prepare_range(ptr: *mut u8) -> std::io::Result<()> {
    use crate::transport::io_uring::task::IDLE_DISCARD_BYTES;
    static CHECK: OnceLock<bool> = OnceLock::new();
    let check = *CHECK.get_or_init(|| std::env::var_os("ORBITE_DIAG_SINGLE_RESIDENCY").is_some());
    let resident = || -> std::io::Result<usize> {
        // SAFETY: sysconf takes a constant key with no pointer parameters.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(std::io::Error::other)?;
        if page == 0 || IDLE_DISCARD_BYTES % page != 0 {
            return Err(std::io::Error::other("unaligned observation range"));
        }
        let mut vec = vec![0; IDLE_DISCARD_BYTES / page];
        // SAFETY: the charged Task retains this exact PMD-aligned private region.
        if unsafe { libc::mincore(ptr.cast(), IDLE_DISCARD_BYTES, vec.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(vec.iter().filter(|b| **b & 1 != 0).count() * page)
    };
    let before = check.then(resident).transpose()?;
    if idle_mode() == "populate" {
        // SAFETY: the cached writable private mapping contains this aligned region,
        // belongs to this admitted Task, and no SQE has yet been submitted.
        if unsafe { libc::madvise(ptr.cast(), IDLE_DISCARD_BYTES, libc::MADV_POPULATE_WRITE) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    if let Some(before) = before {
        let after = resident()?;
        eprintln!(
            r#"SINGLE_RESIDENCY {{"mode":"{}","before":{before},"after":{after},"bytes":{IDLE_DISCARD_BYTES}}}"#,
            idle_mode()
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Usage {
    user: i64,
    system: i64,
    minflt: i64,
    majflt: i64,
}

#[allow(unsafe_code)]
fn usage(who: libc::c_int) -> Option<Usage> {
    let mut r = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the entire rusage on success.
    if unsafe { libc::getrusage(who, r.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: getrusage succeeded and initialized the structure.
    let r = unsafe { r.assume_init() };
    let us = |v: libc::timeval| v.tv_sec * 1_000_000 + v.tv_usec;
    Some(Usage {
        user: us(r.ru_utime),
        system: us(r.ru_stime),
        minflt: r.ru_minflt,
        majflt: r.ru_majflt,
    })
}

#[derive(Clone, Copy)]
struct Stamp {
    process_usage: Usage,
    issuer_usage: Usage,
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
            process_usage: usage(libc::RUSAGE_SELF)?,
            issuer_usage: usage(libc::RUSAGE_THREAD)?,
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
    bounds: Option<[Option<Stamp>; 9]>,
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
            bounds: enabled.then_some([None; 9]),
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
            bounds[5] = bounds[4];
        }
    }

    pub(super) fn emit(&self, pieces: usize) {
        let Some(bounds) = &self.bounds else {
            return;
        };
        let names = [
            "preflight",
            "population",
            "ring_setup",
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
            for (tag, x, y) in [
                ("process", a.process_usage, b.process_usage),
                ("issuer", a.issuer_usage, b.issuer_usage),
            ] {
                fields.push(format!(
                    r#""{name}_{tag}_user_us":{},"{name}_{tag}_system_us":{},"{name}_{tag}_minflt":{},"{name}_{tag}_majflt":{}"#,
                    y.user-x.user,y.system-x.system,y.minflt-x.minflt,y.majflt-x.majflt));
            }
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
