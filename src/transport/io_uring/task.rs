use std::alloc::{Layout, alloc, dealloc};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::Arc;

use io_uring::{opcode, squeue, types};

use super::FileRequest;
use crate::memory::DmaLease;
use crate::{Error, Result, TransferOp};

pub(super) struct Bounce {
    pub(super) ptr: NonNull<u8>,
    storage: Allocation,
}

enum Allocation {
    Heap(Layout),
    Mapping {
        base: NonNull<libc::c_void>,
        capacity: usize,
    },
}

const HUGE_PAGE: usize = 2 << 20;

#[allow(unsafe_code)]
// SAFETY: Bounce uniquely owns stable aligned bytes. Moving it never moves those
// bytes. Pool leases or the task’s CQE protect access for the entire native use.
unsafe impl Send for Bounce {}
impl Bounce {
    #[allow(unsafe_code)]
    pub(super) fn new(length: usize, alignment: usize) -> Result<Self> {
        let layout = Layout::from_size_align(length, alignment).map_err(|_| Error::InvalidRange)?;
        // SAFETY: file admission validated nonzero length, and Layout checked alignment.
        let ptr = NonNull::new(unsafe { alloc(layout) }).ok_or(Error::ResourceExhausted)?;
        Ok(Self {
            ptr,
            storage: Allocation::Heap(layout),
        })
    }

    // Match the destination's offset within a huge page so whole PMDs can
    // move even when the transfer starts inside its registered allocation.
    #[allow(unsafe_code)]
    fn mapped(length: usize, destination: usize) -> Result<Self> {
        let capacity = length.checked_add(HUGE_PAGE).ok_or(Error::InvalidRange)?;
        // SAFETY: private, writable anonymous memory, with no caller-supplied address.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                capacity,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(Error::ResourceExhausted);
        }
        let Some(base) = NonNull::new(raw) else {
            // SAFETY: undo only the mapping just obtained at virtual address zero.
            unsafe { libc::munmap(raw, capacity) };
            return Err(Error::ResourceExhausted);
        };
        let offset = destination.wrapping_sub(raw as usize) % HUGE_PAGE;
        // SAFETY: offset is strictly less than the extra space reserved above.
        let ptr = unsafe { base.cast::<u8>().add(offset) };
        // SAFETY: raw and capacity are the entire private mapping. Uniform advice
        // keeps this payload in one VMA even when its PMD offset is nonzero.
        unsafe { libc::madvise(raw, capacity, libc::MADV_HUGEPAGE) };
        Ok(Self {
            ptr,
            storage: Allocation::Mapping { base, capacity },
        })
    }

    fn matches_destination(&self, destination: usize) -> bool {
        matches!(self.storage, Allocation::Mapping { .. })
            && self.ptr.as_ptr() as usize % HUGE_PAGE == destination % HUGE_PAGE
    }

    // The cache holds one scalar iovec even when part of it is nonresident.
    // Discard only whole 2 MiB units inside this retired buffer. Its ends
    // may be offset relative to the PMD containing the destination address.
    fn idle_discard_start(&self) -> *mut u8 {
        let start = self.ptr.as_ptr() as usize;
        let end = (start + READ_PREFIX_BYTES) & !(HUGE_PAGE - 1);
        self.ptr
            .as_ptr()
            .wrapping_add(end - IDLE_DISCARD_BYTES - start)
    }

    #[allow(unsafe_code)]
    pub(super) fn discard_idle_pages(&self) -> bool {
        // SAFETY: only a drained or never-submitted charge can return a buffer
        // to its pool. The aligned range is strictly inside its exclusive,
        // writable payload, whose previous bytes have no consumer.
        unsafe {
            libc::madvise(
                self.idle_discard_start().cast(),
                IDLE_DISCARD_BYTES,
                libc::MADV_DONTNEED,
            ) == 0
        }
    }

    #[cfg(test)]
    #[allow(unsafe_code)]
    pub(super) fn idle_residency(&self) -> std::io::Result<(usize, usize)> {
        // SAFETY: sysconf reads the process-wide page size without a user pointer.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(std::io::Error::other)?;
        let addr = self.ptr.as_ptr() as usize;
        let base = addr & !(page - 1);
        let len = (addr - base + READ_PREFIX_BYTES).next_multiple_of(page);
        let mut vec = vec![0u8; len / page];
        // SAFETY: base and len cover the live cache allocation rounded to OS
        // pages. vec contains one output byte per page. The pool lock prevents reuse.
        if unsafe { libc::mincore(base as *mut _, len, vec.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let first = (self.idle_discard_start() as usize - base) / page;
        let resident = |pages: &[u8]| pages.iter().filter(|v| **v & 1 != 0).count() * page;
        Ok((
            resident(&vec),
            resident(&vec[first..first + IDLE_DISCARD_BYTES / page]),
        ))
    }

    fn pageable(
        transport: &super::IoUringTransport,
        length: usize,
        alignment: usize,
        direct_read: bool,
    ) -> Result<Self> {
        // Faulting every base page during GUP dominates a large, fresh direct
        // READ buffer. Only advise complete 2 MiB units of private scratch.
        // The kernel can always satisfy this hint with ordinary pages.
        const HUGE: usize = 2 << 20;
        let advise = direct_read && length >= HUGE;
        let buffer = if advise && alignment < HUGE {
            match (transport.allocate)(length, HUGE) {
                Err(Error::ResourceExhausted) => (transport.allocate)(length, alignment)?,
                result => result?,
            }
        } else {
            (transport.allocate)(length, alignment)?
        };
        if advise && buffer.ptr.as_ptr() as usize % HUGE == 0 {
            // SAFETY: the pointer is huge-page aligned and exclusively owned.
            // The hinted extent only includes complete units inside the allocation.
            #[allow(unsafe_code)]
            unsafe {
                libc::madvise(
                    buffer.ptr.as_ptr().cast(),
                    length / HUGE * HUGE,
                    libc::MADV_HUGEPAGE,
                );
            }
        }
        Ok(buffer)
    }
}

impl Drop for Bounce {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        match self.storage {
            Allocation::Heap(layout) => {
                // SAFETY: exact allocator, pointer and layout, before submit or after drain.
                unsafe { dealloc(self.ptr.as_ptr(), layout) };
            }
            Allocation::Mapping { base, capacity } => {
                // SAFETY: the full VMA remains owned even after individual pages move.
                unsafe { libc::munmap(base.as_ptr(), capacity) };
            }
        }
    }
}

enum Scratch {
    Slot(super::pool::Slot),
    Pageable {
        buffer: Bounce,
        _charge: super::pool::Charge,
    },
}
impl Scratch {
    fn acquire(
        transport: &super::IoUringTransport,
        length: usize,
        alignment: usize,
        direct_read: bool,
    ) -> Result<Self> {
        if let Some(slot) = transport.pool.acquire(
            length,
            alignment,
            super::pool::Access::Host,
            super::pool::Slab::host,
        )? {
            return Ok(Self::Slot(slot));
        }
        let charge = transport.pool.charge(length)?;
        let buffer = Bounce::pageable(transport, length, alignment, direct_read)?;
        Ok(Self::Pageable {
            buffer,
            _charge: charge,
        })
    }

    fn address(&self) -> NonNull<u8> {
        match self {
            Self::Slot(slot) => slot.ptr,
            Self::Pageable { buffer, .. } => buffer.ptr,
        }
    }
}

// Keep at most one prefix per engine. The rest of a large scalar READ is
// pageable and drops as soon as this request has its one CQE.
pub(super) const READ_PREFIX_BYTES: usize = 64 << 20;
pub(super) const IDLE_DISCARD_BYTES: usize = 36 << 20;
#[cfg(test)]
pub(super) static PUBLISHED_BY_MOVE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static RECYCLED_BY_MOVE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

struct SplitRead {
    iovecs: [libc::iovec; 2],
    tail: Option<Bounce>,
    charge: super::pool::Charge,
    spare: Option<Bounce>,
    move_pages: bool,
}

impl SplitRead {
    fn new(
        transport: &super::IoUringTransport,
        memory: &DmaLease,
        length: usize,
        alignment: usize,
    ) -> Result<Self> {
        let mut charge = transport.pool.charge_read(length, Some(alignment))?;
        let destination = memory.address().as_ptr() as usize;
        let move_pages = memory.can_publish_file_pages() && destination % alignment == 0;
        let cached = charge
            .prefix
            .take()
            .filter(|b| !move_pages || b.matches_destination(destination));
        let allocate = |length, offset| {
            if move_pages {
                if let Ok(mapping) = Bounce::mapped(length, destination + offset) {
                    return Ok(mapping);
                }
            }
            Bounce::pageable(transport, length, alignment, true)
        };
        let prefix = match cached {
            Some(prefix) => prefix,
            None => allocate(READ_PREFIX_BYTES, 0)?,
        };
        let remaining = length - READ_PREFIX_BYTES;
        let tail = if remaining == 0 {
            None
        } else {
            Some(allocate(remaining, READ_PREFIX_BYTES)?)
        };
        let move_pages = move_pages
            && prefix.matches_destination(destination)
            && tail
                .as_ref()
                .is_none_or(|b| b.matches_destination(destination + READ_PREFIX_BYTES));
        // These initially absent pages receive only an existing resident prefix
        // from the destination after a full completion. No copy populates them.
        let spare = if move_pages {
            Bounce::mapped(READ_PREFIX_BYTES, destination).ok()
        } else {
            None
        };
        let iovecs = [
            libc::iovec {
                iov_base: prefix.ptr.as_ptr().cast(),
                iov_len: READ_PREFIX_BYTES,
            },
            libc::iovec {
                iov_base: tail
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |b| b.ptr.as_ptr().cast()),
                iov_len: remaining,
            },
        ];
        charge.prefix = Some(prefix);
        Ok(Self {
            iovecs,
            tail,
            charge,
            spare,
            move_pages,
        })
    }

    #[allow(unsafe_code)]
    fn publish(&mut self, memory: &DmaLease, length: usize) -> Result<()> {
        if length == memory.len() {
            if let Some(spare) = &self.spare {
                // SAFETY: the CQE confirmed every old destination byte is replaceable.
                // The fresh spare and destination are exclusive private CPU mappings.
                let moved = unsafe {
                    super::page_publish::recycle(memory.address(), spare.ptr, READ_PREFIX_BYTES)?
                };
                #[cfg(test)]
                RECYCLED_BY_MOVE.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
                if moved != 0 {
                    // Keep the old iovec source until its authoritative bytes are published.
                    // Only the spare, containing old pages, can return to the cache.
                    std::mem::swap(&mut self.spare, &mut self.charge.prefix);
                }
            }
        }
        let mut copied = 0;
        for span in self.iovecs.iter() {
            let n = span.iov_len.min(length - copied);
            if n == 0 {
                break;
            }
            let source = NonNull::new(span.iov_base.cast::<u8>())
                .ok_or(Error::Internal("missing direct read buffer"))?;
            // SAFETY: the confirmed prefix fits in the exclusive destination loan.
            let dest = unsafe { memory.address().add(copied) };
            if self.move_pages {
                // SAFETY: both addresses belong to disjoint owned private CPU
                // mappings. Only the single terminal CQE's prefix is published.
                let _moved = unsafe { super::page_publish::publish(source, dest, n)? };
                #[cfg(test)]
                PUBLISHED_BY_MOVE.fetch_add(_moved, std::sync::atomic::Ordering::Relaxed);
            } else {
                // SAFETY: this CQE initialized n bytes in the disjoint scratch range.
                unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), dest.as_ptr(), n) };
            }
            copied += n;
        }
        Ok(())
    }
}

enum HostMemory {
    One(DmaLease),
    SplitRead {
        memory: DmaLease,
        private: Box<SplitRead>,
    },
    Vector {
        _loans: Vec<DmaLease>,
        iovecs: Box<[libc::iovec]>,
        _padding: Option<Scratch>,
    },
}

pub(super) struct Task {
    pub(super) file: Arc<File>,
    memory: Option<HostMemory>,
    length: usize,
    bounce: Option<Scratch>,
    offset: u64,
    pub(super) result: Option<i32>,
    pub(super) late: bool,
}

impl Task {
    #[allow(unsafe_code)]
    pub(super) fn new(
        operation: TransferOp,
        request: &FileRequest,
        transport: &super::IoUringTransport,
    ) -> Result<(Self, libc::iovec)> {
        if request.parts.vectored() {
            let (loans, spans) = request.parts.borrow_ranges(|m, o, n| m.dma_lease(o, n))?;
            let padding_bytes = request
                .parts
                .iter()
                .filter(|part| part.local.is_none())
                .map(|part| part.length)
                .sum::<usize>();
            let padding = if padding_bytes == 0 {
                None
            } else {
                let scratch = Scratch::acquire(transport, padding_bytes, 4096, false)?;
                if operation == TransferOp::Write {
                    // SAFETY: this exclusive scratch lease covers all padding bytes.
                    // A reused slot may contain bytes from an earlier READ.
                    unsafe {
                        scratch.address().as_ptr().write_bytes(0, padding_bytes);
                    }
                }
                Some(scratch)
            };
            let mut scratch_offset = 0;
            let mut iovecs = Vec::with_capacity(spans.len());
            for (part, span) in request.parts.iter().zip(spans) {
                let address = if let Some(span) = span {
                    loans[span.loan]
                        .address()
                        .as_ptr()
                        .wrapping_add(span.offset)
                } else {
                    let scratch = padding
                        .as_ref()
                        .ok_or(Error::Internal("missing file padding scratch"))?;
                    let address = scratch.address().as_ptr().wrapping_add(scratch_offset);
                    scratch_offset += part.length;
                    address
                };
                iovecs.push(libc::iovec {
                    iov_base: address.cast(),
                    iov_len: part.length,
                });
            }
            let (file, alignment) = request.lease.file_for_iovecs(request.offset, &iovecs)?;
            let (bounce, padding) = if request.lease.is_direct_read(operation, &file) {
                // Padding only advances the position in the private READ buffer.
                for (iov, part) in iovecs.iter_mut().zip(request.parts.iter()) {
                    if part.local.is_none() {
                        iov.iov_base = std::ptr::null_mut();
                    }
                }
                drop(padding);
                (
                    Some(Scratch::acquire(
                        transport,
                        request.length,
                        alignment,
                        true,
                    )?),
                    None,
                )
            } else {
                (None, padding)
            };
            let span = libc::iovec {
                iov_base: bounce
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |b| b.address().as_ptr().cast()),
                iov_len: request.length,
            };
            return Ok((
                Self {
                    file,
                    memory: Some(HostMemory::Vector {
                        _loans: loans,
                        iovecs: iovecs.into_boxed_slice(),
                        _padding: padding,
                    }),
                    length: request.length,
                    bounce,
                    offset: request.offset,
                    result: None,
                    late: false,
                },
                span,
            ));
        }
        let part = &request.parts[0];
        let local = part.registered()?;
        let memory = local.dma_lease(part.local_offset, part.length)?;
        // CUDA-owned host memory is directly accessible after dma_lease consumes
        // its producer event. Use buffered I/O for an unaligned view, without a copy.
        #[cfg(feature = "cuda")]
        let direct_host = if local.is_cuda_pinned_host() {
            Some(request.lease.file_for_buffer(
                request.offset,
                request.length,
                memory.address().as_ptr() as usize,
            )?)
        } else {
            None
        };
        #[cfg(not(feature = "cuda"))]
        let direct_host = None;
        let host_pinned = direct_host.is_some();
        let (file, alignment) = match direct_host {
            Some(descriptor) => descriptor,
            None => request.lease.io_file(request.offset, request.length)?,
        };
        let staging_alignment = if !host_pinned && transport.pool.enabled {
            alignment.max(4096)
        } else {
            alignment
        };
        // A direct READ racing truncate can zero the rest of the last block
        // even when its CQE reports a shorter prefix. Keep that write private.
        let protect = request.lease.is_direct_read(operation, &file);
        if protect
            && !transport.pool.enabled
            && request.length >= READ_PREFIX_BYTES
            && READ_PREFIX_BYTES % staging_alignment == 0
        {
            let private = Box::new(SplitRead::new(
                transport,
                &memory,
                request.length,
                staging_alignment,
            )?);
            // Mixed host/device batches advance by this logical host span.
            // Task::entry submits the two private iovecs for the single READV.
            let iov = libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: request.length,
            };
            return Ok((
                Self {
                    file,
                    memory: Some(HostMemory::SplitRead { memory, private }),
                    length: request.length,
                    bounce: None,
                    offset: request.offset,
                    result: None,
                    late: false,
                },
                iov,
            ));
        }
        let bounce = if protect || memory.address().as_ptr() as usize % staging_alignment != 0 {
            let bounce = Scratch::acquire(transport, request.length, staging_alignment, protect)?;
            if operation == TransferOp::Write {
                // SAFETY: the exclusive loan covers all initialized WRITE bytes.
                // The fresh bounce is disjoint and has exactly that capacity.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        memory.address().as_ptr(),
                        bounce.address().as_ptr(),
                        request.length,
                    );
                }
            }
            Some(bounce)
        } else {
            None
        };
        let iov = libc::iovec {
            iov_base: bounce
                .as_ref()
                .map_or(memory.address(), Scratch::address)
                .as_ptr()
                .cast(),
            iov_len: memory.len(),
        };
        Ok((
            Self {
                file,
                memory: Some(HostMemory::One(memory)),
                length: request.length,
                bounce,
                offset: request.offset,
                result: None,
                late: false,
            },
            iov,
        ))
    }

    pub(super) fn bytes(&self) -> usize {
        self.result.unwrap_or(0).max(0) as usize
    }

    pub(super) fn observed_error(&self, operation: TransferOp) -> Option<Error> {
        cqe_error(operation, self.result?, self.length, self.late)
    }

    pub(super) fn entry(
        &self,
        operation: TransferOp,
        iov: &libc::iovec,
        index: usize,
    ) -> squeue::Entry {
        let iovecs = match (&self.memory, &self.bounce) {
            (Some(HostMemory::SplitRead { private, .. }), _) => {
                &private.iovecs[..if private.tail.is_some() { 2 } else { 1 }]
            }
            (Some(HostMemory::Vector { iovecs, .. }), None) => &**iovecs,
            _ => std::slice::from_ref(iov),
        };
        entry(&self.file, self.offset, operation, iovecs, index)
    }
    #[allow(unsafe_code)]
    pub(super) fn complete(&mut self, operation: TransferOp, result: i32, late: bool) -> bool {
        let Some(memory) = &mut self.memory else {
            return false;
        };
        if result > 0 && result as usize > self.length {
            return false;
        }
        if operation == TransferOp::Read && result > 0 {
            let length = result as usize;
            match (memory, &self.bounce) {
                (HostMemory::SplitRead { memory, private }, _) => {
                    if private.publish(memory, length).is_err() {
                        return false;
                    }
                }
                (HostMemory::One(memory), Some(bounce)) => {
                    // SAFETY: this CQE initialized length disjoint scratch bytes.
                    // The exclusive destination loan covers the checked prefix.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            bounce.address().as_ptr(),
                            memory.address().as_ptr(),
                            length,
                        )
                    };
                }
                (HostMemory::Vector { iovecs, .. }, Some(bounce)) => {
                    let mut copied = 0;
                    for iov in iovecs {
                        let n = iov.iov_len.min(length - copied);
                        if n == 0 {
                            break;
                        }
                        if !iov.iov_base.is_null() {
                            // SAFETY: the CQE covers this scratch prefix and the retained
                            // loan covers this destination. Padding has a null address.
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    bounce.address().as_ptr().add(copied),
                                    iov.iov_base.cast(),
                                    n,
                                )
                            };
                        }
                        copied += n;
                    }
                }
                (_, None) => (),
            }
        }
        self.result = Some(result);
        self.late = late;
        // Publish only after releasing this CQE's exclusive range. Another task
        // in the same ring can still be using its own disjoint range.
        self.bounce.take();
        self.memory.take();
        true
    }
}

pub(super) fn entry(
    file: &File,
    offset: u64,
    operation: TransferOp,
    iov: &[libc::iovec],
    index: usize,
) -> squeue::Entry {
    let fd = types::Fd(file.as_raw_fd());
    let entry = match operation {
        TransferOp::Read => opcode::Readv::new(fd, iov.as_ptr(), iov.len() as u32)
            .offset(offset)
            .build(),
        TransferOp::Write => opcode::Writev::new(fd, iov.as_ptr(), iov.len() as u32)
            .offset(offset)
            .build(),
    };
    entry.user_data(index as u64)
}

pub(super) fn cqe_error(
    operation: TransferOp,
    result: i32,
    length: usize,
    late: bool,
) -> Option<Error> {
    match result {
        code if code < 0 => Some(Error::Backend {
            operation: "io_uring CQE",
            code,
        }),
        bytes if bytes as usize != length => Some(Error::Io(std::io::Error::new(
            if operation == TransferOp::Read {
                std::io::ErrorKind::UnexpectedEof
            } else {
                std::io::ErrorKind::WriteZero
            },
            "short io_uring completion",
        ))),
        _ if late => Some(Error::Timeout),
        _ => None,
    }
}
