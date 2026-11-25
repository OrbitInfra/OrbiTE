//! Experimental bounded rolling private READ. Only the worker runs the loop.
//! The release decision additionally needs the single-native-read EIO contract.
use super::{FileAttempts, FileRequest, IoUringTransport, failure, pool, ring::Ring, task::Bounce};
use crate::memory::DmaLease;
use crate::{BatchReport, Error, RemoteEffect, Result, TransferOp};
use io_uring::{opcode, squeue, types};
use std::fs::File;
use std::io;
use std::mem::ManuallyDrop;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Instant;

const SLOTS: usize = 4;
pub(super) const CHUNK: usize = 2 << 20;
const BYTES: usize = SLOTS * CHUNK;

pub(super) struct Window {
    // Ring and its buffer registration retire before free or charge release.
    ring: Box<dyn Ring>,
    private: Bounce,
    _charge: pool::Charge,
    fixed: bool,
}

impl Window {
    fn acquire(owner: &IoUringTransport) -> Result<Self> {
        if let Some(window) = owner
            .idle_read
            .lock()
            .ok()
            .and_then(|mut c| c.rolling.take())
        {
            return Ok(window);
        }
        let charge = owner.pool.charge(BYTES)?;
        let ring = owner.create_ring(SLOTS as u32)?;
        let private = (owner.allocate)(BYTES, CHUNK)?;
        // SAFETY: private owns all complete, aligned pages across BYTES.
        #[allow(unsafe_code)]
        unsafe {
            libc::madvise(private.ptr.as_ptr().cast(), BYTES, libc::MADV_HUGEPAGE);
        }
        let fixed = ring.register_private(private.ptr, BYTES);
        Ok(Self {
            ring,
            private,
            _charge: charge,
            fixed,
        })
    }

    fn address(&self, index: usize) -> *mut u8 {
        self.private
            .ptr
            .as_ptr()
            .wrapping_add(index % SLOTS * CHUNK)
    }

    fn entry(&self, file: &File, offset: u64, index: usize) -> squeue::Entry {
        let fd = types::Fd(file.as_raw_fd());
        let address = self.address(index);
        let at = offset + (index * CHUNK) as u64;
        if self.fixed {
            opcode::ReadFixed::new(fd, address, CHUNK as u32, 0)
                .offset(at)
                .build()
        } else {
            opcode::Read::new(fd, address, CHUNK as u32)
                .offset(at)
                .build()
        }
        .user_data(index as u64)
    }
}

struct InFlight {
    window: Window,
    file: Arc<File>,
    destination: DmaLease,
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[target_feature(enable = "avx2")]
unsafe fn stream(src: *const u8, dst: *mut u8) {
    use std::arch::x86_64::*;
    // SAFETY: dispatch checked alignment, both ranges cover CHUNK disjoint bytes.
    unsafe {
        for at in (0..CHUNK).step_by(256) {
            for lane in 0..8 {
                let i = at + lane * 32;
                _mm256_stream_si256(dst.add(i).cast(), _mm256_load_si256(src.add(i).cast()));
            }
        }
        _mm_sfence();
    }
}

impl InFlight {
    #[allow(unsafe_code)]
    fn publish(&self, index: usize, count: usize) {
        let src = self.window.address(index);
        let dst = self
            .destination
            .address()
            .as_ptr()
            .wrapping_add(index * CHUNK);
        #[cfg(target_arch = "x86_64")]
        if count == CHUNK && dst as usize % 32 == 0 && std::is_x86_feature_detected!("avx2") {
            // SAFETY: the ordered CQE initialized this entire private aligned slot;
            // request admission and the exclusive DmaLease cover the destination.
            unsafe {
                stream(src, dst);
            }
            return;
        }
        // SAFETY: CQE supplied the exact initialized byte count. The destination
        // remains under an exclusive lease, and points to a disjoint allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst, count);
        }
    }
}

pub(super) fn execute(
    owner: &IoUringTransport,
    req: &FileRequest,
    deadline: Instant,
    progress: Option<(&FileAttempts<'_>, usize)>,
    report: &mut BatchReport,
) -> Result<bool> {
    // Other shapes and configured alternative routes retain their single-request semantics.
    if req.length != 64 << 20
        || req.parts.vectored()
        || req.selection.has_fallback()
        || owner.pool.enabled
    {
        return Ok(false);
    }
    let (file, alignment) = req.lease.io_file(req.offset, CHUNK)?;
    if !req.lease.is_direct_read(TransferOp::Read, &file) || CHUNK % alignment != 0 {
        return Ok(false);
    }
    let part = &req.parts[0];
    let local = part.registered()?;
    // Preserve the existing buffered selection for an unaligned pinned-host view.
    #[cfg(feature = "cuda")]
    if local.is_cuda_pinned_host()
        && (local.base_address() + part.local_offset as u64) % alignment as u64 != 0
    {
        return Ok(false);
    }
    let destination = local.dma_lease(part.local_offset, part.length)?;
    if destination.len() != req.length {
        return Err(Error::InvalidRange);
    }
    let window = Window::acquire(owner)?;
    let until = deadline
        .checked_add(crate::transport::DESTRUCTION_TIMEOUT)
        .ok_or(Error::InvalidRange)?;
    crate::engine::remaining(deadline)?;
    let mut live = ManuallyDrop::new(InFlight {
        window,
        file,
        destination,
    });
    let mut from = 0;
    let mut posted = 0;
    let mut prefix = 0;
    let mut results = [None; SLOTS];
    let mut error = None;
    let mut pending_submit_error = None;
    let mut quarantine = None;
    let total = req.length / CHUNK;
    while from < total && (from < posted || (error.is_none() && pending_submit_error.is_none())) {
        if error.is_none() && pending_submit_error.is_none() && posted < total {
            if Instant::now() >= deadline {
                pending_submit_error = Some(Error::Timeout);
            } else if progress.is_some_and(|(p, i)| p.cancel_requested(i)) {
                pending_submit_error = Some(Error::Cancelled);
            } else {
                let end = (from + SLOTS).min(total);
                if end > posted {
                    let entries: Vec<_> = (posted..end)
                        .map(|i| live.window.entry(&live.file, req.offset, i))
                        .collect();
                    if let Err(e) = live.window.ring.push(&entries) {
                        pending_submit_error = Some(e.into());
                    } else {
                        let drain =
                            super::submit_ring(&mut *live.window.ring, entries.len(), deadline);
                        if let Err(code) = drain.status {
                            quarantine = Some(code);
                            break;
                        }
                        posted += drain.consumed;
                        if drain.consumed > 0 {
                            if let Some((p, i)) = progress {
                                p.posted(i);
                            }
                        }
                        if drain.consumed < entries.len() {
                            pending_submit_error = Some(Error::Backend {
                                operation: "io_uring submit",
                                code: drain.submit_error,
                            });
                        }
                    }
                }
            }
        }
        if from == posted {
            break;
        }
        while results[from % SLOTS].is_none() {
            let receipt = match live.window.ring.peek() {
                Ok(Some(c)) => Ok(c),
                Ok(None) => live.window.ring.reap(until),
                Err(e) => Err(e),
            };
            match receipt {
                Ok((id, n))
                    if id >= from as u64
                        && id < (posted as u64)
                        && n <= CHUNK as i32
                        && results[id as usize % SLOTS].is_none() =>
                {
                    results[id as usize % SLOTS] = Some(n)
                }
                Ok(_) => {
                    quarantine = Some(-libc::EPROTO);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted && Instant::now() < until => {
                    continue;
                }
                Err(e) => {
                    quarantine = Some(-e.raw_os_error().unwrap_or(libc::EIO));
                    break;
                }
            }
        }
        if quarantine.is_some() {
            break;
        }
        let Some(n) = results[from % SLOTS].take() else {
            quarantine = Some(-libc::EPROTO);
            break;
        };
        if error.is_none() {
            if n > 0 {
                live.publish(from, n as usize);
                prefix += n as usize;
            }
            if n < CHUNK as i32 {
                error = super::task::cqe_error(TransferOp::Read, n, CHUNK, false);
            }
        }
        from += 1;
    }
    let mut fault = quarantine
        .map(super::unquiesced)
        .or(error)
        .or(pending_submit_error);
    if fault.is_none() && Instant::now() >= deadline {
        fault = Some(Error::Timeout);
    }
    let drained = quarantine.is_none();
    if drained {
        let mut live = ManuallyDrop::into_inner(live);
        drop(live.destination);
        if fault.is_none() && live.window.ring.empty_after_drain() {
            if let Ok(mut cache) = owner.idle_read.lock() {
                cache.save_rolling(live.window);
            }
        }
    }
    if let Some(e) = fault {
        if let Some((p, i)) = progress {
            p.fail_file_task(i, prefix, e.for_report(), RemoteEffect::None, drained);
        }
        report.failure = Some(failure(0, e, RemoteEffect::None));
        report.locally_drained = drained;
    } else {
        if let Some((p, i)) = progress {
            p.complete_task(i, prefix);
        }
        report.completed_requests = 1;
        report.transferred_bytes = prefix;
    }
    Ok(true)
}
