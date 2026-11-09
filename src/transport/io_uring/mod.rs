//! Single-issuer file sub-batches. The caller or existing batch worker owns the ring.
mod config;
pub(crate) mod pool;
pub use config::IoUringConfig;
pub use pool::IoUringStats;
mod ring;
#[cfg(feature = "cuda")]
mod staging;
mod task;

#[cfg(test)]
#[path = "../../../tests/unit/transport/io_uring.rs"]
pub(crate) mod tests;

use std::io;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::time::Instant;

use io_uring::squeue;

use self::ring::Ring;
use self::task::Task;
use crate::engine::file_attempt::FileAttempts;
use crate::engine::remaining;
use crate::transport::DESTRUCTION_TIMEOUT;
use crate::{BatchReport, Error, RemoteEffect, Result, TransferFailure, TransferOp};

use crate::segment::file::FileRequest;

#[derive(Clone)]
pub(crate) struct IoUringTransport {
    available: Arc<std::sync::OnceLock<bool>>,
    pool: Arc<pool::Pool>,
    allocate: fn(usize, usize) -> Result<task::Bounce>,
    create: Arc<dyn Fn(u32) -> io::Result<Box<dyn Ring>> + Send + Sync>,
}

impl IoUringTransport {
    pub(crate) fn probe(config: &crate::EngineConfig) -> Result<Option<Self>> {
        Ok(Some(Self {
            available: Arc::default(),
            pool: pool::Pool::new(config.io_uring, config.max_inflight_bytes)?,
            create: Arc::new(ring::create),
            allocate: task::Bounce::new,
        }))
    }
    pub(crate) fn available(&self) -> bool {
        *self.available.get_or_init(|| (self.create)(2).is_ok())
    }
    pub(crate) fn stats(&self) -> IoUringStats {
        self.pool.stats()
    }
    pub(crate) fn close(&self) -> Result<()> {
        self.pool.close()
    }

    pub(crate) fn execute(
        &self,
        operation: TransferOp,
        requests: &[FileRequest],
        deadline: Instant,
        progress: Option<(&FileAttempts<'_>, usize)>,
        report: &mut BatchReport,
        #[cfg(feature = "cuda")] (proxy, max_batch_requests): (
            &Arc<crate::runtime::proxy_manager::ProxyManager>,
            usize,
        ),
    ) {
        if requests.is_empty() {
            return;
        }
        let count = progress.map_or(requests.len(), |(p, first)| {
            p.begin_file_group(first, requests.len())
        });
        let mut run = || -> Result<()> {
            remaining(deadline)?;
            #[cfg(feature = "cuda")]
            let staged = requests[..count].iter().any(|r| r.parts.has_cuda());
            #[cfg(not(feature = "cuda"))]
            let staged = false;
            #[cfg(feature = "cuda")]
            if staged {
                staging::Submission {
                    requests: &requests[..count],
                    proxy,
                    pool: &self.pool,
                    operation,
                    deadline,
                    progress,
                }
                .execute(self, max_batch_requests, report)?;
            }
            if count != 0 && !staged {
                *report = self
                    .start_host(operation, &requests[..count], requests.len(), deadline)?
                    .finish(operation, progress);
            }
            if report.failure.is_none() && count != requests.len() {
                report.failure = Some(failure(count, Error::Cancelled, RemoteEffect::None));
            }
            Ok(())
        };
        if let Err(error) = run() {
            report.locally_drained = !matches!(error, Error::Unquiesced { .. });
            report.failure = Some(failure(0, error, RemoteEffect::None));
        }
    }

    pub(crate) fn start_host(
        &self,
        operation: TransferOp,
        requests: &[FileRequest],
        capacity: usize,
        deadline: Instant,
    ) -> Result<Submitted> {
        let count = requests.len();
        let mut tasks = Vec::new();
        tasks
            .try_reserve_exact(count)
            .map_err(|_| Error::ResourceExhausted)?;
        let mut iovecs = Vec::new();
        iovecs
            .try_reserve_exact(count)
            .map_err(|_| Error::ResourceExhausted)?;
        for request in requests {
            let (task, iov) = Task::new(operation, request, self)?;
            tasks.push(task);
            iovecs.push(iov);
        }
        let size = u32::try_from(capacity).map_err(|_| Error::ResourceExhausted)?;
        let mut ring = (self.create)(size)?;
        if ring.space() < count {
            return Err(Error::ResourceExhausted);
        }
        // The iovec array is immutable after this point, separate from
        // task state the worker mutates as receipts arrive.
        let entries: Vec<_> = tasks
            .iter()
            .zip(&iovecs)
            .enumerate()
            .map(|(index, (task, iov))| task.entry(operation, iov, index))
            .collect();
        remaining(deadline)?;
        let batch = LiveBatch {
            ring,
            tasks,
            _iovecs: iovecs,
        };
        batch.start(&entries, deadline)
    }
}

fn failure(index: usize, error: Error, effect: RemoteEffect) -> TransferFailure {
    TransferFailure {
        request_index: index,
        affected_requests: 1,
        error,
        remote_effect: effect,
    }
}

struct LiveBatch {
    ring: Box<dyn Ring>,
    tasks: Vec<Task>,
    _iovecs: Vec<libc::iovec>,
}

struct Drain {
    // Only this prefix was consumed. If SQ accounting itself is invalid,
    // every published SQE must be treated as potentially in flight.
    consumed: usize,
    submit_error: i32,
    status: std::result::Result<(), i32>,
}

impl Drain {
    fn invalid_sq(count: usize) -> Self {
        Self {
            consumed: count,
            submit_error: -libc::EPROTO,
            status: Err(-libc::EPROTO),
        }
    }

    fn missing_error(&self, index: usize) -> Error {
        if index < self.consumed {
            if let Err(code) = self.status {
                return unquiesced(code);
            }
        }
        Error::Backend {
            operation: "io_uring submit",
            code: self.submit_error,
        }
    }

    fn effect(&self, op: TransferOp, index: usize) -> RemoteEffect {
        if op == TransferOp::Write && index < self.consumed {
            RemoteEffect::Unknown
        } else {
            RemoteEffect::None
        }
    }
}

fn unquiesced(code: i32) -> Error {
    Error::Unquiesced {
        operation: "io_uring drain",
        code,
    }
}

impl LiveBatch {
    fn start(self, entries: &[squeue::Entry], deadline: Instant) -> Result<Submitted> {
        let drain_deadline =
            deadline
                .checked_add(DESTRUCTION_TIMEOUT)
                .ok_or(Error::InvalidConfig(
                    "file drain deadline overflows monotonic clock",
                ))?;
        let mut live = ManuallyDrop::new(self);
        if let Err(error) = live.ring.push(entries) {
            // push_multiple is all-or-nothing and no enter has taken place.
            drop(ManuallyDrop::into_inner(live));
            return Err(error.into());
        }
        let count = live.tasks.len();
        let drain = submit_ring(&mut *live.ring, count, deadline);
        Ok(Submitted {
            live,
            drain,
            deadline,
            drain_deadline,
        })
    }
}

// Dropping this value without a receipt deliberately retains the native owner.
// The batch owner must retain its admission too if a worker cannot take over.
pub(crate) struct Submitted {
    live: ManuallyDrop<LiveBatch>,
    drain: Drain,
    deadline: Instant,
    drain_deadline: Instant,
}
#[allow(unsafe_code)]
// SAFETY: the single issuer moves exclusive ownership to the batch worker only
// after enter returns. The Send ring, immutable iovecs, fd and native memory
// loans stay at stable addresses until CQEs prove completion or are quarantined.
unsafe impl Send for Submitted {}

impl Submitted {
    /// Peek, then briefly wait for the single-request caller's completion.
    /// Expiry or an enter error hands the unchanged owner to the drain worker;
    /// neither is a release receipt or permission to submit the SQE again.
    pub(crate) fn complete_small(
        mut self,
        operation: TransferOp,
        progress: Option<(&FileAttempts<'_>, usize)>,
    ) -> std::result::Result<BatchReport, Self> {
        if self.drain.status.is_err() || self.drain.consumed == 0 {
            return Ok(self.report(operation, progress));
        }
        if self.drain.consumed == 1 {
            let receipt = match self.live.ring.peek() {
                Ok(Some(cqe)) => Some(cqe),
                Ok(None) => self.wait_small(),
                Err(_) => None,
            };
            if let Some((id, result)) = receipt {
                if id != 0
                    || !observe(
                        &mut self.live.tasks,
                        0,
                        result,
                        operation,
                        self.deadline,
                        progress,
                    )
                {
                    self.drain.status = Err(-libc::EPROTO);
                }
                return Ok(self.report(operation, progress));
            }
        }
        Err(self)
    }

    fn wait_small(&mut self) -> Option<(u64, i32)> {
        // A blocked enter yields the CPU. Keep both the time and interruption
        // budget fixed, independent of the destruction window used by drain.
        let until = (Instant::now() + SMALL_WAIT).min(self.deadline);
        for _ in 0..3 {
            if Instant::now() >= until {
                break;
            }
            match self.live.ring.reap(until) {
                Ok(cqe) => return Some(cqe),
                Err(e) if matches!(e.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) => {}
                Err(_) => break,
            }
        }
        None
    }

    pub(crate) fn finish(
        mut self,
        operation: TransferOp,
        progress: Option<(&FileAttempts<'_>, usize)>,
    ) -> BatchReport {
        if self.drain.status.is_ok() {
            let live = &mut *self.live;
            self.drain.status = drain(
                &mut *live.ring,
                self.drain.consumed,
                self.drain_deadline,
                |index, result| {
                    observe(
                        &mut live.tasks,
                        index,
                        result,
                        operation,
                        self.deadline,
                        progress,
                    )
                },
            );
        }
        self.report(operation, progress)
    }

    fn report(
        self,
        operation: TransferOp,
        progress: Option<(&FileAttempts<'_>, usize)>,
    ) -> BatchReport {
        let Self { live, drain, .. } = self;
        let mut report = BatchReport {
            completed_requests: 0,
            transferred_bytes: 0,
            failure: None,
            locally_drained: true,
            notification: crate::NotificationStatus::NotRequested,
        };
        report.locally_drained = drain.status.is_ok();
        // CQEs already published independent task outcomes. Finish any tasks
        // without a receipt, including the tail a failed enter never consumed.
        if let Some((p, first)) = progress {
            for (index, _) in live
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.result.is_none())
            {
                if index < drain.consumed {
                    p.posted(first + index);
                }
                p.fail_file_task(
                    first + index,
                    0,
                    drain.missing_error(index),
                    drain.effect(operation, index),
                    report.locally_drained || index >= drain.consumed,
                );
            }
        }
        // The group report, like a network report, counts only the successful prefix.
        for (index, task) in live.tasks.iter().enumerate() {
            let error = match task.result {
                None => Some(drain.missing_error(index)),
                Some(_) => task.observed_error(operation),
            };
            if let Some(reason) = error {
                report.failure = Some(TransferFailure {
                    request_index: index,
                    affected_requests: drain.consumed.saturating_sub(index).max(1),
                    error: drain.status.err().map_or(reason, unquiesced),
                    remote_effect: drain.effect(operation, index),
                });
                break;
            }
            report.completed_requests += 1;
            report.transferred_bytes += task.bytes();
        }
        if report.locally_drained {
            drop(ManuallyDrop::into_inner(live));
        }
        // Otherwise the ring, pointed-to iovecs, exclusive loans, fds and bounce buffers
        // remain live. The shared submission owner also retains admission and file leases.
        report
    }
}

const SMALL_WAIT: std::time::Duration = std::time::Duration::from_micros(200);

fn observe(
    tasks: &mut [Task],
    index: usize,
    result: i32,
    operation: TransferOp,
    deadline: Instant,
    progress: Option<(&FileAttempts<'_>, usize)>,
) -> bool {
    let task = &mut tasks[index];
    if !task.complete(operation, result, Instant::now() >= deadline) {
        return false;
    }
    if let Some((p, first)) = progress {
        p.posted(first + index);
        if let Some(error) = task.observed_error(operation) {
            let effect = if operation == TransferOp::Write {
                RemoteEffect::Unknown
            } else {
                RemoteEffect::None
            };
            p.fail_file_task(first + index, task.bytes(), error, effect, true);
        } else {
            p.complete_task(first + index, task.bytes());
        }
    }
    true
}

#[cfg(feature = "cuda")]
fn submit_and_drain(
    ring: &mut dyn Ring,
    count: usize,
    deadline: Instant,
    drain_deadline: Instant,
    observe: impl FnMut(usize, i32) -> bool,
) -> Drain {
    let mut result = submit_ring(ring, count, deadline);
    if result.status.is_ok() {
        result.status = drain(ring, result.consumed, drain_deadline, observe);
    }
    result
}

fn submit_ring(ring: &mut dyn Ring, count: usize, deadline: Instant) -> Drain {
    let mut submitted = 0;
    let mut retries = 0;
    let mut code = -libc::EIO;
    while submitted < count {
        match ring.submit((count - submitted) as u32) {
            Ok(n) if n > 0 && n <= count - submitted => {
                submitted += n;
                retries = 0;
            }
            Ok(0) if retries < 3 => retries += 1,
            Ok(0) => break,
            Ok(_) => return Drain::invalid_sq(count),
            Err(e)
                if matches!(e.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) && retries < 3 =>
            {
                retries += 1;
            }
            Err(e) => {
                code = -e.raw_os_error().unwrap_or(libc::EIO);
                break;
            }
        }
        if submitted < count && Instant::now() >= deadline {
            code = -libc::ETIMEDOUT;
            break;
        }
    }
    // Even a failed enter must not erase a prefix already consumed from the SQ.
    let Some(consumed) = count.checked_sub(ring.queued()).filter(|n| *n >= submitted) else {
        return Drain::invalid_sq(count);
    };
    Drain {
        consumed,
        submit_error: code,
        status: Ok(()),
    }
}

fn drain(
    ring: &mut dyn Ring,
    consumed: usize,
    drain_deadline: Instant,
    mut observe: impl FnMut(usize, i32) -> bool,
) -> std::result::Result<(), i32> {
    let mut polling = false;
    for _ in 0..consumed {
        let (id, result) = loop {
            if Instant::now() >= drain_deadline {
                return Err(-libc::ETIMEDOUT);
            }
            if polling {
                match ring.peek() {
                    Ok(Some(cqe)) => break cqe,
                    Ok(None) => {
                        std::thread::sleep(
                            drain_deadline
                                .saturating_duration_since(Instant::now())
                                .min(std::time::Duration::from_millis(1)),
                        );
                        continue;
                    }
                    Err(e) => return Err(-e.raw_os_error().unwrap_or(libc::EIO)),
                }
            }
            match ring.reap(drain_deadline) {
                Ok(cqe) => break cqe,
                // A wakeup or interrupted wait never starts a new window.
                Err(e)
                    if e.kind() == io::ErrorKind::Interrupted
                        || e.kind() == io::ErrorKind::TimedOut
                        || e.raw_os_error() == Some(libc::ETIME) =>
                {
                    continue;
                }
                Err(e) => {
                    log::warn!("io_uring completion wait failed ({e}), polling the mapped CQ");
                    polling = true;
                }
            }
        };
        let Ok(index) = usize::try_from(id) else {
            return Err(-libc::EPROTO);
        };
        if index >= consumed || !observe(index, result) {
            return Err(-libc::EPROTO);
        }
    }
    Ok(())
}
