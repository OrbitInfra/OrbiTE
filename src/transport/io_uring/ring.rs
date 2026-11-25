use std::io;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use io_uring::{EnterFlags, IoUring, squeue, types};

// One submitting/reaping owner at a time and a plain ring (no SQPOLL). Only an explicit
// submit consumes SQEs. Waiting for a CQE must never submit a failed tail.
pub(super) trait Ring: Send {
    fn space(&mut self) -> usize;
    fn queued(&mut self) -> usize;
    fn push(&mut self, entries: &[squeue::Entry]) -> io::Result<()>;
    fn submit(&mut self, count: u32) -> io::Result<usize>;
    fn peek(&mut self) -> io::Result<Option<(u64, i32)>>;
    fn reap(&mut self, until: Instant) -> io::Result<(u64, i32)>;
    fn register_private(&self, _ptr: std::ptr::NonNull<u8>, _bytes: usize) -> bool {
        false
    }
    // Only a plain native ring can be reused after every submitted operation drained.
    fn empty_after_drain(&mut self) -> bool {
        false
    }
    // Adapters without a non-consuming CQ count defer the group to the worker.
    fn batch_ready(&mut self, _count: u32, _until: Instant) -> bool {
        false
    }
}

#[derive(Default)]
pub(super) struct IdleReadRing {
    ring: Option<Box<dyn Ring>>,
    closed: bool,
    pub(super) rolling: Option<super::rolling::Window>,
}

impl IdleReadRing {
    pub(super) fn take(&mut self, count: usize) -> Option<Box<dyn Ring>> {
        let mut ring = self.ring.take()?;
        (ring.space() >= count).then_some(ring)
    }

    pub(super) fn evict(&mut self) -> bool {
        let large = self.rolling.take().is_some();
        self.ring.take().is_some() || large
    }

    pub(super) fn put(&mut self, mut ring: Box<dyn Ring>) {
        if !self.closed
            && ring.empty_after_drain()
            && self
                .ring
                .as_mut()
                .is_none_or(|idle| idle.space() < ring.space())
        {
            self.ring = Some(ring);
        }
    }

    pub(super) fn save_rolling(&mut self, ring: super::rolling::Window) {
        if !self.closed && self.rolling.is_none() {
            self.rolling = Some(ring);
        }
    }

    pub(super) fn close(&mut self) {
        self.closed = true;
        self.rolling.take();
        self.ring.take();
    }
}

pub(super) fn create(count: u32) -> io::Result<Box<dyn Ring>> {
    #[cfg(test)]
    super::tests::RING_SETUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Keep default completion interrupts: the caller may hand reaping to its
    // worker. COOP_TASKRUN/DEFER_TASKRUN would require a different progress proof.
    Ok(Box::new(IoUring::new(count)?))
}

#[allow(unsafe_code)]
impl Ring for IoUring {
    fn register_private(&self, ptr: std::ptr::NonNull<u8>, bytes: usize) -> bool {
        let iov = libc::iovec {
            iov_base: ptr.as_ptr().cast(),
            iov_len: bytes,
        };
        // SAFETY: Window owns this disjoint allocation until its ring is quiescent and closed.
        unsafe { self.submitter().register_buffers(&[iov]) }.is_ok()
    }
    fn empty_after_drain(&mut self) -> bool {
        self.submission().is_empty() && self.completion().is_empty()
    }

    fn space(&mut self) -> usize {
        let sq = self.submission();
        sq.capacity() - sq.len()
    }

    fn queued(&mut self) -> usize {
        self.submission().len()
    }

    fn push(&mut self, entries: &[squeue::Entry]) -> io::Result<()> {
        // SAFETY: each SQE refers to its live submission's fd, iovec and exclusive
        // memory loan or bounce buffer. The owner will drain or quarantine all of them.
        unsafe { self.submission().push_multiple(entries) }
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOSPC))
    }

    fn submit(&mut self, count: u32) -> io::Result<usize> {
        // SAFETY: count covers only published SQEs in this single-issuer ring.
        unsafe { self.submitter().enter::<libc::sigset_t>(count, 0, 0, None) }
    }

    fn peek(&mut self) -> io::Result<Option<(u64, i32)>> {
        Ok(self
            .completion()
            .next()
            .map(|cqe| (cqe.user_data(), cqe.result())))
    }

    fn reap(&mut self, until: Instant) -> io::Result<(u64, i32)> {
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            if let Some(cqe) = self.completion().next() {
                return Ok((cqe.user_data(), cqe.result()));
            }
            if self.params().is_feature_ext_arg() {
                wait_completions(self, 1, wait)?;
            } else {
                wait_readable(self, wait)?;
            }
        }
    }
    fn batch_ready(&mut self, count: u32, until: Instant) -> bool {
        if self.completion().len() >= count as usize {
            return true;
        }
        let wait = until.saturating_duration_since(Instant::now());
        if wait.is_zero() || !self.params().is_feature_ext_arg() {
            return false;
        }
        wait_completions(self, count, wait).is_ok() && self.completion().len() >= count as usize
    }
}

#[allow(unsafe_code)]
fn wait_completions(ring: &IoUring, count: u32, wait: Duration) -> io::Result<usize> {
    let ts = types::Timespec::from(wait);
    let args = types::SubmitArgs::new().timespec(&ts);
    // SAFETY: EXT_ARG selects SubmitArgs' kernel layout, and ts lives through
    // enter. Zero submissions keeps any unconsumed SQ tail untouched.
    unsafe {
        ring.submitter().enter(
            0,
            count,
            (EnterFlags::GETEVENTS | EnterFlags::EXT_ARG).bits(),
            Some(&args),
        )
    }
}

// Kernels before EXT_ARG expose CQ readiness on the plain ring fd.
#[allow(unsafe_code)]
pub(super) fn wait_readable(ring: &IoUring, wait: Duration) -> io::Result<()> {
    let mut fd = libc::pollfd {
        fd: ring.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = libc::timespec {
        tv_sec: wait.as_secs().try_into().unwrap_or(libc::time_t::MAX),
        tv_nsec: wait.subsec_nanos() as _,
    };
    // SAFETY: fd and timeout remain valid for this one-fd wait; no signal mask is supplied.
    let n = unsafe { libc::ppoll(&mut fd, 1, &timeout, std::ptr::null()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n > 0 && fd.revents & libc::POLLIN == 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}
